// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! `banlieue bootstrap cloud-hypervisor-host`: the first credential for an
//! External provider's host (ADR-0060 Decision 5).
//!
//! Run by an admin, against the cluster, once per host. It requests a bound
//! token for the ServiceAccount the operator created for that Provider, and
//! writes two files for the host:
//!
//! - `kubeconfig` — the cluster's server and CA, and a user that reads its
//!   token from a **file** on the host (`tokenFile`), never inline;
//! - `token` — that token.
//!
//! Both `0600`. Copy them to the host (`scripts/ch-host-provider-up.sh`
//! installs them). From then on the provider renews the token itself; the
//! kubeconfig never changes. A host that stays down longer than one token
//! lifetime needs this run again.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use banlieue_api::banlieue::{Provider, ProviderClass, ProviderDeployment};
use banlieue_provider_sdk::naming::workload_name;
use clap::Args;
use k8s_openapi::api::authentication::v1::{TokenRequest, TokenRequestSpec};
use k8s_openapi::api::core::v1::ServiceAccount;
use kube::Client;
use kube::api::{Api, PostParams};
use kube::config::Kubeconfig;
use serde_json::json;

/// Where the bootstrap script puts the token on a host.
pub const DEFAULT_HOST_TOKEN_PATH: &str = "/etc/banlieue/credentials/token";
/// Default token lifetime, in minutes: 24 h (ADR-0060 Decision 5).
const DEFAULT_LIFETIME_MINUTES: u32 = 24 * 60;
/// The API server's floor for a requested token lifetime.
const MIN_LIFETIME_MINUTES: u32 = 10;
const SECONDS_PER_MINUTE: i64 = 60;
/// Files hold a credential: the owner only.
const FILE_MODE: u32 = 0o600;
const DIR_MODE: u32 = 0o700;
const KUBECONFIG_FILE: &str = "kubeconfig";
const TOKEN_FILE: &str = "token";
/// Names used inside the generated kubeconfig.
const CLUSTER_ENTRY: &str = "banlieue";
const USER_ENTRY: &str = "provider";

/// `banlieue bootstrap cloud-hypervisor-host`.
#[derive(Debug, Args)]
pub struct HostCredentialArgs {
    /// The host's `Provider` (one host is one Provider, ADR-0060).
    #[arg(long)]
    pub provider: String,

    /// The Provider's namespace. External providers live in the operator's
    /// install namespace, where it may grant token renewal.
    #[arg(long, default_value = "banlieue-system")]
    pub namespace: String,

    /// Directory to write `kubeconfig` and `token` into. Created `0700`.
    #[arg(long)]
    pub output_dir: PathBuf,

    /// Where the token will live on the host; written into the kubeconfig.
    #[arg(long, default_value = DEFAULT_HOST_TOKEN_PATH)]
    pub host_token_path: PathBuf,

    /// Token lifetime in minutes (default 24 h; the API server's minimum is
    /// 10). The provider renews at half-life with its own 24 h default, so a
    /// short first token proves renewal quickly.
    #[arg(long, default_value_t = DEFAULT_LIFETIME_MINUTES)]
    pub lifetime_minutes: u32,
}

/// The host's kubeconfig: `server` and `ca_data` (base64 PEM, as in a
/// kubeconfig) from the admin's context, and a user reading its token from
/// `token_path`. Never an inline token, so it holds no secret and never
/// needs rewriting.
#[must_use]
pub fn host_kubeconfig(server: &str, ca_data: Option<&str>, token_path: &Path) -> String {
    let mut cluster = json!({ "server": server });
    if let Some(ca) = ca_data {
        cluster["certificate-authority-data"] = json!(ca);
    }
    let doc = json!({
        "apiVersion": "v1",
        "kind": "Config",
        "clusters": [{ "name": CLUSTER_ENTRY, "cluster": cluster }],
        "users": [{ "name": USER_ENTRY, "user": { "tokenFile": token_path.display().to_string() } }],
        "contexts": [{ "name": USER_ENTRY, "context": { "cluster": CLUSTER_ENTRY, "user": USER_ENTRY } }],
        "current-context": USER_ENTRY,
    });
    serde_yaml::to_string(&doc).unwrap_or_default()
}

/// Write `contents` to a new `0600` file at `path`.
///
/// # Errors
/// The I/O error, including an existing file: never overwrite a credential
/// silently.
pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(path)
        .with_context(|| format!("creating {} (it must not exist yet)", path.display()))?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

/// The admin context's server and CA data, to put in the host's kubeconfig.
fn admin_cluster() -> Result<(String, Option<String>)> {
    let kc = Kubeconfig::read().context("reading your kubeconfig (KUBECONFIG)")?;
    let context = kc
        .current_context
        .as_deref()
        .context("your kubeconfig has no current-context")?;
    let cluster_name = kc
        .contexts
        .iter()
        .find(|c| c.name == context)
        .and_then(|c| c.context.as_ref())
        .map(|c| c.cluster.clone())
        .context("current context not found")?;
    let cluster = kc
        .clusters
        .iter()
        .find(|c| c.name == cluster_name)
        .and_then(|c| c.cluster.as_ref())
        .context("current context's cluster not found")?;
    let server = cluster.server.clone().context("cluster has no server")?;
    Ok((server, cluster.certificate_authority_data.clone()))
}

/// Issue the host's first credential.
///
/// # Errors
/// The Provider or its class is missing or not External, the operator has
/// not created the ServiceAccount yet, the token request fails, or an
/// output file already exists.
pub async fn run(args: &HostCredentialArgs) -> Result<()> {
    let client = Client::try_default()
        .await
        .context("connecting with your kubeconfig")?;
    let provider = Api::<Provider>::namespaced(client.clone(), &args.namespace)
        .get(&args.provider)
        .await
        .with_context(|| format!("Provider {}/{}", args.namespace, args.provider))?;
    let class_name = provider.spec.provider_class_ref.name.clone();
    let class = Api::<ProviderClass>::all(client.clone())
        .get(&class_name)
        .await
        .with_context(|| format!("ProviderClass {class_name}"))?;
    if class.spec.deployment_mode() != ProviderDeployment::External {
        bail!(
            "ProviderClass {class_name} is not External; only an External provider runs on a \
             host with an issued credential (ADR-0060 Decision 3)"
        );
    }
    let sa = workload_name(&class_name, &args.provider);
    let accounts: Api<ServiceAccount> = Api::namespaced(client.clone(), &args.namespace);
    if accounts.get_opt(&sa).await?.is_none() {
        bail!(
            "ServiceAccount {}/{sa} does not exist yet — the operator creates it for an External \
             Provider; is banlieue-operator running?",
            args.namespace
        );
    }
    if args.lifetime_minutes < MIN_LIFETIME_MINUTES {
        bail!("--lifetime-minutes must be at least {MIN_LIFETIME_MINUTES}");
    }
    let req = TokenRequest {
        spec: Some(TokenRequestSpec {
            audiences: None,
            expiration_seconds: Some(i64::from(args.lifetime_minutes) * SECONDS_PER_MINUTE),
            ..Default::default()
        }),
        ..Default::default()
    };
    let token = accounts
        .create_token_request(&sa, &PostParams::default(), &req)
        .await
        .with_context(|| format!("requesting a token for {}/{sa}", args.namespace))?
        .status
        .and_then(|s| s.token)
        .context("the API server returned no token")?;

    let (server, ca) = admin_cluster()?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(&args.output_dir)
        .with_context(|| format!("creating {}", args.output_dir.display()))?;
    write_private(
        &args.output_dir.join(KUBECONFIG_FILE),
        &host_kubeconfig(&server, ca.as_deref(), &args.host_token_path),
    )?;
    write_private(&args.output_dir.join(TOKEN_FILE), &token)?;
    println!(
        "Wrote {0}/{KUBECONFIG_FILE} and {0}/{TOKEN_FILE} for ServiceAccount {1}/{sa}.\n\
         Install them on the host (scripts/ch-host-provider-up.sh), token at {2}.",
        args.output_dir.display(),
        args.namespace,
        args.host_token_path.display()
    );
    Ok(())
}

#[cfg(test)]
#[path = "host_credential_tests.rs"]
mod host_credential_tests;
