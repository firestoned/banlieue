// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Integration test against a **real vCenter** over the production transport.
//!
//! Every other test in this crate drives `FakeClient`, which validates the
//! reconciler's logic but proves nothing about the wire: whether the JSON
//! transport negotiates, whether TLS and the CA bundle are wired correctly,
//! and whether `vim_rs`'s view of vCenter's object model matches the real
//! thing. A self-consistent misunderstanding round-trips against a fake
//! forever and only fails on contact with a real server.
//!
//! **This deliberately replaces a vcsim-based suite.** `vim_rs`'s
//! `vcsim_compat` feature requires its `xml` feature — the SOAP transport —
//! while the workspace pins `vim_rs` with `default-features = false` because
//! production vCenter uses JSON (ADR-0009). A vcsim test would therefore
//! exercise the transport production does *not* use. See ADR-0014's
//! follow-ups: backend connectivity is validated here, against the real thing.
//!
//! `#[ignore]`d so `cargo test` stays hermetic. Run it explicitly:
//!
//! ```sh
//! VSPHERE_ENDPOINT=https://bar.foo.io/sdk \
//! VSPHERE_USERNAME='svc-banlieue@vsphere.local' \
//! VSPHERE_PASSWORD='…' \
//!   cargo test -p banlieue-provider-vsphere --test live_vcenter -- --ignored --nocapture
//! ```
//!
//! or `make vsphere-live-test`.
//!
//! Optional:
//!
//! - `VSPHERE_INSECURE=true` — skip TLS verification (self-signed lab certs).
//! - `VSPHERE_CA_BUNDLE=/path/to/ca.pem` — trust a private CA instead. Prefer
//!   this over `VSPHERE_INSECURE`; it exercises the ADR-0008 BYOC path, which
//!   is what production installs actually use.
//! - `VSPHERE_TEMPLATE=<name>` — also exercise template lookup.
//!
//! Credentials come from the environment and are never written down here: this
//! is a public repository (`.claude/rules/no-real-infrastructure.md`), and
//! `bar.foo.io` above is a placeholder, not a host.

use banlieue_api::banlieue::ProviderConnection;
use banlieue_api::common::LocalObjectReference;
use banlieue_provider_vsphere::client::{
    Credentials, VSphereClient, VSphereClientFactory, VimClientFactory,
    install_default_crypto_provider,
};

/// Everything the harness needs, or `None` when the environment is not set up.
struct Settings {
    connection: ProviderConnection,
    credentials: Credentials,
    ca_bundle_pem: Option<String>,
}

fn settings() -> Option<Settings> {
    let endpoint = std::env::var("VSPHERE_ENDPOINT").ok()?;
    let username = std::env::var("VSPHERE_USERNAME").ok()?;
    let password = std::env::var("VSPHERE_PASSWORD").ok()?;

    let insecure = std::env::var("VSPHERE_INSECURE")
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
        .unwrap_or(false);

    let ca_bundle_pem = std::env::var("VSPHERE_CA_BUNDLE").ok().map(|path| {
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading VSPHERE_CA_BUNDLE {path}: {e}"))
    });

    Some(Settings {
        connection: ProviderConnection {
            endpoint,
            // Unused on this path: the factory takes the resolved PEM directly,
            // because credential/CA resolution belongs to the reconciler where
            // the kube client lives (ADR-0008).
            credentials_ref: LocalObjectReference {
                name: "unused-in-this-harness".to_string(),
            },
            insecure_skip_tls_verify: insecure,
            ca_bundle: None,
        },
        credentials: Credentials { username, password },
        ca_bundle_pem,
    })
}

fn require_settings() -> Settings {
    settings().unwrap_or_else(|| {
        panic!(
            "set VSPHERE_ENDPOINT, VSPHERE_USERNAME and VSPHERE_PASSWORD to run this test\n  \
             e.g. VSPHERE_ENDPOINT=https://bar.foo.io/sdk VSPHERE_USERNAME=svc@vsphere.local \
             VSPHERE_PASSWORD=... cargo test -p banlieue-provider-vsphere --test live_vcenter \
             -- --ignored"
        )
    })
}

async fn connect(settings: &Settings) -> Box<dyn VSphereClient> {
    // reqwest 0.13's `rustls-no-provider` PANICS with "No provider set" at the
    // first TLS use unless a process-default CryptoProvider is installed. The
    // binary does this at startup (ADR-0009); a test binary must do it too.
    install_default_crypto_provider();

    VimClientFactory::new()
        .build(
            &settings.connection,
            &settings.credentials,
            settings.ca_bundle_pem.as_deref(),
        )
        .await
        .expect("connecting to vCenter failed")
}

/// Connect and walk the inventory the `Provider` reconciler depends on.
///
/// This is the assertion that matters: it is the only place the JSON transport,
/// the TLS/BYOC path, and `vim_rs`'s decoding of vCenter's object model are all
/// exercised together against a real server.
#[tokio::test]
#[ignore = "requires a real vCenter; set VSPHERE_ENDPOINT / VSPHERE_USERNAME / VSPHERE_PASSWORD"]
async fn connect_and_walk_inventory_against_real_vcenter() {
    let settings = require_settings();
    eprintln!("connecting to {} ...", settings.connection.endpoint);
    let client = connect(&settings).await;
    eprintln!("  connected (TLS + login ok)");

    let datacenters = client
        .list_datacenters()
        .await
        .expect("listing datacenters failed");
    eprintln!("  datacenters ({}):", datacenters.len());
    for dc in &datacenters {
        eprintln!("    {:<24} {}", dc.name, dc.moref);
    }

    // Environment-independent: any vCenter worth pointing this at has at least
    // one datacenter. A decode that silently went wrong shows up as empty
    // names or empty morefs rather than as an error, so check those explicitly.
    assert!(
        !datacenters.is_empty(),
        "expected at least one datacenter; an empty list usually means the account \
         cannot see the inventory rather than that the inventory is empty"
    );
    for dc in &datacenters {
        assert!(!dc.name.is_empty(), "datacenter name decoded empty");
        assert!(
            !dc.moref.is_empty(),
            "datacenter {} has an empty moref — the reconciler uses this to scope \
             every subsequent lookup",
            dc.name
        );
    }

    // Clusters are what become failure domains, so an empty or mis-decoded
    // cluster list is what a Provider with no schedulable capacity looks like.
    let mut total_clusters = 0;
    for dc in &datacenters {
        let clusters = client
            .list_clusters(dc)
            .await
            .unwrap_or_else(|e| panic!("listing clusters in {}: {e}", dc.name));
        eprintln!("  clusters in {} ({}):", dc.name, clusters.len());
        for cluster in &clusters {
            eprintln!("    {:<24} {}", cluster.name, cluster.moref);
            assert!(!cluster.name.is_empty(), "cluster name decoded empty");
            assert!(
                !cluster.moref.is_empty(),
                "cluster {} has an empty moref",
                cluster.name
            );
            assert_eq!(
                cluster.datacenter_moref, dc.moref,
                "cluster {} is not linked back to the datacenter it was listed from; \
                 the scheduler relies on that link to build failure domains",
                cluster.name
            );
        }
        total_clusters += clusters.len();
    }

    assert!(
        total_clusters > 0,
        "no clusters found in any datacenter — a Provider here would publish no \
         failure domains and nothing could ever be scheduled onto it"
    );
    eprintln!(
        "  {total_clusters} cluster(s) across {} datacenter(s)",
        datacenters.len()
    );
}

/// Template lookup, which `VMImage` readiness depends on.
///
/// Separate because it needs a template name that exists in *your* inventory,
/// and there is no environment-independent default.
#[tokio::test]
#[ignore = "requires a real vCenter and VSPHERE_TEMPLATE naming a template in it"]
async fn find_template_against_real_vcenter() {
    let settings = require_settings();
    let template_name = std::env::var("VSPHERE_TEMPLATE")
        .expect("set VSPHERE_TEMPLATE to the name of a template in your inventory");

    let client = connect(&settings).await;
    let datacenters = client
        .list_datacenters()
        .await
        .expect("listing datacenters");

    let mut found = None;
    for dc in &datacenters {
        if let Some(template) = client
            .find_template(dc, None, &template_name)
            .await
            .unwrap_or_else(|e| panic!("searching {} for {template_name}: {e}", dc.name))
        {
            eprintln!(
                "  found {template_name} in {}: moref={}",
                dc.name, template.moref
            );
            found = Some(template);
            break;
        }
    }

    let template = found.unwrap_or_else(|| {
        panic!(
            "template {template_name} not found in any of {} datacenter(s)",
            datacenters.len()
        )
    });
    assert!(!template.moref.is_empty(), "template moref decoded empty");
}

/// A wrong name must return `None`, not an error and not a false positive.
///
/// Guards the "not found" path the `VMImage` reconciler branches on — if it
/// surfaced as an error instead, an image would report a hard failure rather
/// than `TemplateNotFound`.
#[tokio::test]
#[ignore = "requires a real vCenter; set VSPHERE_ENDPOINT / VSPHERE_USERNAME / VSPHERE_PASSWORD"]
async fn a_missing_template_is_none_rather_than_an_error() {
    let settings = require_settings();
    let client = connect(&settings).await;
    let datacenters = client
        .list_datacenters()
        .await
        .expect("listing datacenters");
    let dc = datacenters.first().expect("at least one datacenter");

    let result = client
        .find_template(dc, None, "banlieue-definitely-does-not-exist-0000")
        .await;

    match result {
        Ok(None) => eprintln!("  absent template correctly reported as None"),
        Ok(Some(t)) => panic!("vCenter returned a template for a bogus name: {}", t.moref),
        Err(e) => panic!("a missing template must be Ok(None), not an error: {e}"),
    }
}
