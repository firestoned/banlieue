// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Live protocol tier for `banlieue-aws` (ADR-0174; `rules/testing.md`).
//!
//! Proves that EC2 and STS accept what the client sends and that the client
//! reads what they answer. Every test is `#[ignore]`d: running one is an
//! explicit request to talk to AWS, so a missing variable or a refused call
//! is a **failure**, never a skip.
//!
//! Credentials come either the way production gets them, a ServiceAccount
//! token exchanged at STS (ADR-0159), or from an operator's own temporary
//! session (ADR-0159 Decision 4's fallback). Give the role only the actions
//! the provider would hold, so a missing permission fails here, not in a
//! cluster.
//!
//! ```sh
//! # Production path: a projected token exchanged at STS.
//! kubectl create token <provider-sa> -n <ns> \
//!   --audience sts.amazonaws.com --duration 10m > "$TOKEN_FILE"
//! AWS_REGION=us-east-1 \
//! AWS_ROLE_ARN=arn:aws:iam::111122223333:role/banlieue-ec2 \
//! AWS_WEB_IDENTITY_TOKEN_FILE="$TOKEN_FILE" \
//!   cargo test -p banlieue-aws --test live_ec2 -- --ignored --test-threads=1
//!
//! # Or an operator session (AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY and
//! # AWS_SESSION_TOKEN all set); `web_identity_exchange` still needs the
//! # three variables above and fails without them.
//! ```
//!
//! `reads_zones_offerings_and_instance_types` takes `AWS_INSTANCE_TYPE`
//! (default `t3.small`). `run_observe_terminate` launches one instance and
//! always terminates it; it also needs `AWS_IMAGE_ID` (a UEFI AMI with
//! NitroTPM), `AWS_SUBNET_ID`, `AWS_SECURITY_GROUP_ID` and `AWS_ZONE` (a
//! zone name in the subnet's zone), and takes `AWS_ROOT_DEVICE` (default
//! `/dev/xvda`) and `AWS_ROOT_GIB` (default 10). Its tags carry
//! `banlieue-purpose=roadmap22-24-25-spike` so a tag-scoped policy can
//! allow it. Nothing printed names an account, an ARN or a whole
//! instance ID.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use banlieue_aws::api::Ec2Api;
use banlieue_aws::console::{ReadyState, ready_state};
use banlieue_aws::instance::{EkKeyType, RunInstancesRequest, VolumeType, client_token_for};
use banlieue_aws::sigv4::sha256_hex;
use banlieue_aws::tags::{OwnershipTags, TAG_OWNER_UID};
use banlieue_aws::types::InstanceState;
use banlieue_aws::{
    CredentialSource, Ec2Client, StaticSource, WebIdentityConfig, WebIdentitySource,
};

const ENV_REGION: &str = "AWS_REGION";
const ENV_ROLE_ARN: &str = "AWS_ROLE_ARN";
const ENV_TOKEN_FILE: &str = "AWS_WEB_IDENTITY_TOKEN_FILE";
const ENV_ACCESS_KEY_ID: &str = "AWS_ACCESS_KEY_ID";
const ENV_SECRET_ACCESS_KEY: &str = "AWS_SECRET_ACCESS_KEY";
const ENV_SESSION_TOKEN: &str = "AWS_SESSION_TOKEN";

const DEFAULT_INSTANCE_TYPE: &str = "t3.small";
const DEFAULT_ROOT_DEVICE: &str = "/dev/xvda";
const DEFAULT_ROOT_GIB: u32 = 10;
const DEFAULT_SESSION_NAME: &str = "banlieue-live.ec2-live";
const LIVE_KIND: &str = "EC2Machine";
const LIVE_NAMESPACE: &str = "banlieue-live";
const LIVE_PROVIDER: &str = "ec2-live";
const PURPOSE_TAG: &str = "banlieue-purpose";
const PURPOSE: &str = "roadmap22-24-25-spike";
const USER_DATA: &[u8] = b"#!/bin/sh\necho banlieue-live-ec2\n";

/// RunInstances to `running`: the spike's launches took well under this.
const RUNNING_TIMEOUT: Duration = Duration::from_secs(600);
/// TerminateInstances to `shutting-down` or `terminated`.
const TERMINATE_TIMEOUT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// How long the EK may stay unpublished after `running` before the test fails.
const EK_TIMEOUT: Duration = Duration::from_secs(300);
/// Characters of an instance ID printed: `i-0abc`.
const SHOWN_ID_CHARS: usize = 6;
/// A UID's random bytes and its `8-4-4-4-12` hex groups.
const UID_BYTES: usize = 16;
const UID_GROUPS: [usize; 5] = [8, 4, 4, 4, 12];

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn env(name: &str) -> String {
    var(name).unwrap_or_else(|| panic!("set {name}; see this file's module docs"))
}

fn env_or(name: &str, default: &str) -> String {
    var(name).unwrap_or_else(|| default.to_string())
}

fn install_provider() {
    let _ = banlieue_tls::install_default_crypto_provider();
}

/// `i-0abc...`: enough to match a console line, not the whole ID.
fn short(instance_id: &str) -> String {
    let prefix: String = instance_id.chars().take(SHOWN_ID_CHARS).collect();
    format!("{prefix}...")
}

/// A fresh random UUID-shaped owner UID, from aws-lc-rs's generator.
fn random_uid() -> String {
    let mut bytes = [0u8; UID_BYTES];
    SystemRandom::new().fill(&mut bytes).expect("system random");
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut groups = Vec::new();
    let mut at = 0;
    for len in UID_GROUPS {
        groups.push(&hex[at..at + len]);
        at += len;
    }
    groups.join("-")
}

fn web_identity_source() -> WebIdentitySource {
    install_provider();
    let missing: Vec<&str> = [ENV_REGION, ENV_ROLE_ARN, ENV_TOKEN_FILE]
        .into_iter()
        .filter(|n| var(n).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "web identity needs {}; see this file's module docs",
        missing.join(", ")
    );
    let config = WebIdentityConfig::from_lookup(var, DEFAULT_SESSION_NAME).expect("config");
    WebIdentitySource::new(config).expect("web identity source")
}

/// Web identity when its variables are set, else the operator's session;
/// neither is a failure naming what is missing.
fn credential_source() -> Arc<dyn CredentialSource> {
    install_provider();
    if var(ENV_ROLE_ARN).is_some() && var(ENV_TOKEN_FILE).is_some() {
        return Arc::new(web_identity_source());
    }
    let session_vars = [ENV_ACCESS_KEY_ID, ENV_SECRET_ACCESS_KEY, ENV_SESSION_TOKEN];
    let missing: Vec<&str> = session_vars
        .into_iter()
        .filter(|n| var(n).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "no credentials: set {ENV_ROLE_ARN} and {ENV_TOKEN_FILE}, or all of \
         {ENV_ACCESS_KEY_ID}, {ENV_SECRET_ACCESS_KEY}, {ENV_SESSION_TOKEN} (missing: {})",
        missing.join(", ")
    );
    Arc::new(StaticSource::from_lookup(var).expect("static credentials"))
}

fn client() -> Ec2Client {
    Ec2Client::new(&env(ENV_REGION), credential_source()).expect("client")
}

#[tokio::test]
#[ignore = "talks to EC2; see module docs"]
async fn reads_zones_offerings_and_instance_types() {
    let client = client();
    let instance_type = env_or("AWS_INSTANCE_TYPE", DEFAULT_INSTANCE_TYPE);

    let zones = client.availability_zones().await.expect("zones");
    assert!(!zones.is_empty(), "the region has no zones");
    for zone in &zones {
        assert!(!zone.name.is_empty() && !zone.id.is_empty(), "{zone:?}");
        println!("zone {} = {} ({})", zone.name, zone.id, zone.state);
    }

    let types = vec![instance_type.clone()];
    let offerings = client
        .instance_type_offerings(&types)
        .await
        .expect("offerings");
    assert!(
        offerings.iter().any(|o| o.instance_type == instance_type),
        "{instance_type} is offered in no zone"
    );
    let locations: Vec<&str> = offerings.iter().map(|o| o.location.as_str()).collect();
    println!("{instance_type} offered in {locations:?}");

    let infos = client.instance_types(&types).await.expect("instance types");
    let info = infos
        .iter()
        .find(|i| i.instance_type == instance_type)
        .expect("the type is described");
    assert!(info.default_vcpus > 0, "{info:?}");
    assert!(!info.architectures.is_empty(), "{info:?}");
    println!(
        "{instance_type}: {} vCPUs, {:?}, NitroTPM {} {:?}, boot modes {:?}",
        info.default_vcpus,
        info.architectures,
        info.nitro_tpm_supported,
        info.nitro_tpm_versions,
        info.boot_modes
    );
    assert!(!client.throttling_sustained());
}

#[tokio::test]
#[ignore = "talks to STS and EC2; see module docs"]
async fn web_identity_exchange() {
    let source = web_identity_source();
    let first = source.credentials().await.expect("exchange");
    assert!(!first.access_key_id.is_empty());
    assert!(!first.secret_access_key.is_empty());
    assert!(first.session_token.is_some(), "a role session has a token");
    // Cached: no second exchange, the same session.
    let again = source.credentials().await.expect("cached");
    assert_eq!(again.access_key_id, first.access_key_id);

    // The session signs a call EC2 accepts.
    let client = Ec2Client::new(&env(ENV_REGION), Arc::new(source)).expect("client");
    let zones = client
        .availability_zones()
        .await
        .expect("signed with the session");
    assert!(!zones.is_empty());
}

fn run_request(uid: &str) -> RunInstancesRequest {
    let zone = env("AWS_ZONE");
    let owner = OwnershipTags::new(uid, LIVE_KIND, LIVE_NAMESPACE, LIVE_PROVIDER);
    let name = format!("banlieue-live-{}", &uid[..UID_GROUPS[0]]);
    let mut tags = owner
        .resource_tags(Some(&name), None, &BTreeMap::new())
        .expect("tags");
    // In banlieue's own namespace, so set here rather than as an extra tag.
    tags.insert(PURPOSE_TAG.to_string(), PURPOSE.to_string());
    let root_gib = env_or("AWS_ROOT_GIB", &DEFAULT_ROOT_GIB.to_string())
        .parse()
        .expect("AWS_ROOT_GIB is a whole number");
    RunInstancesRequest {
        image_id: env("AWS_IMAGE_ID"),
        instance_type: env_or("AWS_INSTANCE_TYPE", DEFAULT_INSTANCE_TYPE),
        client_token: client_token_for(uid, &zone),
        zone_name: zone,
        subnet_id: env("AWS_SUBNET_ID"),
        security_group_ids: vec![env("AWS_SECURITY_GROUP_ID")],
        root_device_name: env_or("AWS_ROOT_DEVICE", DEFAULT_ROOT_DEVICE),
        root_volume_size_gib: root_gib,
        volume_type: VolumeType::Gp3,
        iops: None,
        throughput: None,
        kms_key_id: None,
        user_data: USER_DATA.to_vec(),
        instance_tags: tags.clone(),
        volume_tags: tags.clone(),
        network_interface_tags: tags,
    }
}

/// Everything between the create and the terminate; an error is reported
/// after the instance is gone.
async fn observe(client: &Ec2Client, instance_id: &str, uid: &str) -> Result<(), String> {
    let shown = short(instance_id);
    let deadline = Instant::now() + RUNNING_TIMEOUT;
    loop {
        // Not visible yet just after the create is `None`, not gone.
        let seen = client
            .describe_instance(instance_id)
            .await
            .map_err(|e| format!("describe {shown}: {e}"))?;
        let state = seen.map(|i| i.state);
        if state == Some(InstanceState::Running) {
            break;
        }
        if state.as_ref().is_some_and(InstanceState::is_gone) {
            return Err(format!("{shown} went to {state:?} before running"));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{shown} not running after {RUNNING_TIMEOUT:?}: {state:?}"
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    println!("{shown} running");

    let console = client
        .console_output(instance_id)
        .await
        .map_err(|e| format!("console: {e}"))?;
    let ready = ready_state(&console, instance_id) == ReadyState::Ready;
    println!(
        "console: {} bytes, ready marker present: {ready}",
        console.len()
    );

    // EC2 answers `InstanceTpmEkPub.NotFound` for a while after `running`
    // (seen live 2026-11-27): the EK is published some time into boot, so
    // not-found here means "not yet" and is retried to a deadline.
    let running_at = Instant::now();
    for key_type in [EkKeyType::Rsa2048, EkKeyType::EccSecP384] {
        let deadline = Instant::now() + EK_TIMEOUT;
        let ek = loop {
            match client.tpm_ek_pub(instance_id, key_type).await {
                Ok(ek) => break ek,
                Err(e) if e.is_not_found() && Instant::now() < deadline => {
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
                Err(e) => return Err(format!("EK {}: {e}", key_type.as_str())),
            }
        };
        println!(
            "EK {} ({}): sha256 {}, readable {:?} after running",
            key_type.as_str(),
            ek.key_format,
            sha256_hex(&ek.key_value),
            running_at.elapsed()
        );
    }

    client
        .disable_instance_metadata(instance_id)
        .await
        .map_err(|e| format!("disable IMDS: {e}"))?;
    println!("IMDS disabled");

    let found = client
        .find_instances_by_tag(TAG_OWNER_UID, uid)
        .await
        .map_err(|e| format!("find by owner tag: {e}"))?;
    if !found.iter().any(|i| i.instance_id == instance_id) {
        return Err(format!("{shown} not found by its owner tag"));
    }
    println!("found by owner tag ({} instance(s))", found.len());
    Ok(())
}

/// Terminates and waits for `shutting-down` or `terminated`.
async fn terminate(client: &Ec2Client, instance_id: &str) -> Result<(), String> {
    let shown = short(instance_id);
    let state = client
        .terminate_instance(instance_id)
        .await
        .map_err(|e| format!("terminate {shown}: {e}"))?;
    println!("terminate {shown}: {state:?}");
    let deadline = Instant::now() + TERMINATE_TIMEOUT;
    loop {
        let state = client
            .describe_instance(instance_id)
            .await
            .map_err(|e| format!("describe {shown}: {e}"))?
            .map(|i| i.state);
        if matches!(
            state,
            None | Some(InstanceState::ShuttingDown | InstanceState::Terminated)
        ) {
            println!("{shown}: {state:?}");
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{shown} still {state:?} after {TERMINATE_TIMEOUT:?}"
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
#[ignore = "launches and terminates one EC2 instance; see module docs"]
async fn run_observe_terminate() {
    let client = client();
    let uid = random_uid();
    let request = run_request(&uid);

    let instance_id = match client.run_instance(&request).await {
        Ok(id) => id,
        Err(e) => {
            // A create that failed after EC2 accepted it may still have
            // left an instance: sweep by the owner tag before failing.
            if let Ok(left) = client.find_instances_by_tag(TAG_OWNER_UID, &uid).await {
                for instance in left {
                    let _ = terminate(&client, &instance.instance_id).await;
                }
            }
            panic!("RunInstances: {e}");
        }
    };
    println!("launched {}", short(&instance_id));

    let outcome = observe(&client, &instance_id, &uid).await;
    // Always, whatever `observe` found.
    let terminated = terminate(&client, &instance_id).await;

    outcome.expect("observe");
    terminated.expect("terminate");
}
