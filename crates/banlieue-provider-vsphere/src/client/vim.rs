// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Production `VSphereClient` implementation backed by `vim_rs`.
//!
//! Phase 1B iteration 1 surface: connect (basic-auth + optional insecure TLS),
//! list datacenters, list clusters per datacenter. Iteration 2 grows it with
//! datastores, networks, and the VSphereMachine VM-lifecycle calls.

use std::sync::Arc;

use async_trait::async_trait;
use banlieue_api::banlieue::ProviderConnection;
use tracing::debug;
use vim_rs::core::client::{Client, ClientBuilder};
use vim_rs::mo::cluster_compute_resource::ClusterComputeResource;
use vim_rs::mo::container_view::ContainerView;
use vim_rs::mo::datacenter::Datacenter as VimDatacenter;
use vim_rs::mo::view_manager::ViewManager;
use vim_rs::mo::virtual_machine::VirtualMachine as VimVirtualMachine;
use vim_rs::types::structs::ManagedObjectReference;

use crate::error::{Error, Result};

use super::{Cluster, Credentials, Datacenter, Template, VSphereClient, VSphereClientFactory};

const APP_NAME: &str = env!("CARGO_PKG_NAME");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

const MO_TYPE_DATACENTER: &str = "Datacenter";
const MO_TYPE_CLUSTER: &str = "ClusterComputeResource";
const MO_TYPE_VIRTUAL_MACHINE: &str = "VirtualMachine";

/// Factory that talks to a real vCenter via vim_rs.
#[derive(Default, Clone)]
pub struct VimClientFactory;

impl VimClientFactory {
    pub fn new() -> Self {
        Self
    }
}

/// Install the rustls **ring** crypto provider as the process default.
///
/// reqwest 0.13 is built with `rustls-no-provider` (ADR-0009): it resolves the
/// TLS provider via `CryptoProvider::get_default()` and **panics with
/// `"No provider set"`** on first TLS use if none was installed. We install ring
/// — the same provider kube/hyper-rustls already use — so the whole process
/// shares one crypto stack (no aws-lc-rs, no OpenSSL).
///
/// Idempotent and safe to call from multiple roles: `install_default` returns
/// `Err` if a provider is already set, which we deliberately ignore. Call once
/// at startup, before any TLS (kube client or vCenter) is constructed.
pub fn install_default_crypto_provider() {
    // Ignore the error: a second install (or one already done by another role)
    // is a no-op for our purposes — we only need *a* ring provider to be active.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Parse a PEM bundle into reqwest root certificates.
///
/// Uses `Certificate::from_pem_bundle` (not `from_pem`) so a `caBundle` holding
/// a chain / multiple concatenated CAs contributes every certificate, not just
/// the first. Pure and side-effect-free so it is unit-testable without a vCenter.
///
/// Rejects a bundle that yields **zero** certificates. `from_pem_bundle` returns
/// `Ok([])` for input containing no PEM blocks (e.g. garbage, or an
/// accidentally-empty ConfigMap value); accepting it would silently fall back to
/// the system trust roots while the operator believes they pinned a CA. Failing
/// closed surfaces the misconfiguration on `Provider.status` instead.
///
/// # Errors
/// Returns [`Error::Vsphere`] if the PEM cannot be parsed or contains no
/// certificates.
fn root_certs_from_pem(pem: &str) -> Result<Vec<reqwest::Certificate>> {
    let certs = reqwest::Certificate::from_pem_bundle(pem.as_bytes())
        .map_err(|e| Error::Vsphere(format!("caBundle: invalid PEM: {e}")))?;
    if certs.is_empty() {
        return Err(Error::Vsphere(
            "caBundle: no certificates found in PEM (expected at least one BEGIN CERTIFICATE block)"
                .to_string(),
        ));
    }
    Ok(certs)
}

/// Build the reqwest client banlieue injects into vim_rs (BYOC, ADR-0008).
///
/// banlieue owns the transport so it owns TLS trust: a resolved `ca_bundle_pem`
/// is added as root certificate(s); `insecure` disables verification entirely.
/// The two are independent — a CA bundle does not imply skipping verification —
/// but `insecure` is the bigger hammer and is applied regardless.
///
/// # Errors
/// Returns [`Error::Vsphere`] if the PEM is invalid or the client fails to build.
fn build_http_client(ca_bundle_pem: Option<&str>, insecure: bool) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().user_agent(format!("{APP_NAME}/{APP_VERSION}"));
    if let Some(pem) = ca_bundle_pem {
        for cert in root_certs_from_pem(pem)? {
            builder = builder.add_root_certificate(cert);
        }
    }
    if insecure {
        builder = builder
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true);
    }
    builder
        .build()
        .map_err(|e| Error::Vsphere(format!("build http client: {e}")))
}

#[async_trait]
impl VSphereClientFactory for VimClientFactory {
    async fn build(
        &self,
        connection: &ProviderConnection,
        creds: &Credentials,
        ca_bundle_pem: Option<&str>,
    ) -> Result<Box<dyn VSphereClient>> {
        debug!(
            endpoint = %connection.endpoint,
            ca_bundle = ca_bundle_pem.is_some(),
            insecure = connection.insecure_skip_tls_verify,
            "vim_rs ClientBuilder::new (BYOC)"
        );
        // BYOC (ADR-0008/0009): banlieue builds the reqwest client and hands it to
        // vim_rs, so it owns CA trust / insecure policy end to end. In vim_rs 0.5
        // with `default-client` off, the client is a required 2nd argument to
        // `ClientBuilder::new` (there is no `.http_client()` setter); vim_rs never
        // constructs a reqwest client of its own.
        let http = build_http_client(ca_bundle_pem, connection.insecure_skip_tls_verify)?;
        let client = ClientBuilder::new(&connection.endpoint, http)
            .basic_authn(&creds.username, &creds.password)
            .build()
            .await
            .map_err(|e| Error::Vsphere(format!("connect: {e}")))?;
        Ok(Box::new(VimClientImpl { client }))
    }
}

/// Real vim_rs-backed client. Holds an `Arc<Client>` from the builder; the
/// `Drop` impl logs out automatically when the last `Arc` is dropped.
pub struct VimClientImpl {
    client: Arc<Client>,
}

#[async_trait]
impl VSphereClient for VimClientImpl {
    async fn list_datacenters(&self) -> Result<Vec<Datacenter>> {
        let sc = self.client.service_content();
        let view_manager_moref = sc
            .view_manager
            .as_ref()
            .ok_or(Error::Missing("ServiceContent.view_manager"))?;
        let vm = ViewManager::new(self.client.clone(), &view_manager_moref.value);

        let view_ref = vm
            .create_container_view(
                &sc.root_folder,
                Some(&[MO_TYPE_DATACENTER.to_string()]),
                true,
            )
            .await
            .map_err(|e| Error::Vsphere(format!("create_container_view(Datacenter): {e}")))?;
        let view = ContainerView::new(self.client.clone(), &view_ref.value);

        let morefs = view
            .view()
            .await
            .map_err(|e| Error::Vsphere(format!("ContainerView.view: {e}")))?
            .unwrap_or_default();

        // Destroy the view eagerly so vCenter doesn't accumulate ghost views.
        // Ignore destroy errors — they're not fatal for the caller.
        let _ = view.destroy_view().await;

        let mut out = Vec::with_capacity(morefs.len());
        for moref in morefs {
            let dc = VimDatacenter::new(self.client.clone(), &moref.value);
            let name = dc
                .name()
                .await
                .map_err(|e| Error::Vsphere(format!("Datacenter.name({}): {e}", moref.value)))?;
            out.push(Datacenter {
                name,
                moref: moref.value,
            });
        }
        Ok(out)
    }

    async fn list_clusters(&self, dc: &Datacenter) -> Result<Vec<Cluster>> {
        let sc = self.client.service_content();
        let view_manager_moref = sc
            .view_manager
            .as_ref()
            .ok_or(Error::Missing("ServiceContent.view_manager"))?;
        let vm = ViewManager::new(self.client.clone(), &view_manager_moref.value);

        // Scope the container view to the Datacenter so we only see its clusters.
        let dc_moref = ManagedObjectReference {
            r#type: vim_rs::types::enums::MoTypesEnum::Datacenter,
            value: dc.moref.clone(),
        };
        let view_ref = vm
            .create_container_view(&dc_moref, Some(&[MO_TYPE_CLUSTER.to_string()]), true)
            .await
            .map_err(|e| Error::Vsphere(format!("create_container_view(Cluster): {e}")))?;
        let view = ContainerView::new(self.client.clone(), &view_ref.value);

        let morefs = view
            .view()
            .await
            .map_err(|e| Error::Vsphere(format!("ContainerView.view: {e}")))?
            .unwrap_or_default();
        let _ = view.destroy_view().await;

        let mut out = Vec::with_capacity(morefs.len());
        for moref in morefs {
            let cluster = ClusterComputeResource::new(self.client.clone(), &moref.value);
            let name = cluster
                .name()
                .await
                .map_err(|e| Error::Vsphere(format!("Cluster.name({}): {e}", moref.value)))?;
            out.push(Cluster {
                name,
                moref: moref.value,
                datacenter_moref: dc.moref.clone(),
            });
        }
        Ok(out)
    }

    async fn find_template(&self, dc: &Datacenter, name: &str) -> Result<Option<Template>> {
        let sc = self.client.service_content();
        let view_manager_moref = sc
            .view_manager
            .as_ref()
            .ok_or(Error::Missing("ServiceContent.view_manager"))?;
        let vm = ViewManager::new(self.client.clone(), &view_manager_moref.value);

        let dc_moref = ManagedObjectReference {
            r#type: vim_rs::types::enums::MoTypesEnum::Datacenter,
            value: dc.moref.clone(),
        };
        let view_ref = vm
            .create_container_view(
                &dc_moref,
                Some(&[MO_TYPE_VIRTUAL_MACHINE.to_string()]),
                true,
            )
            .await
            .map_err(|e| Error::Vsphere(format!("create_container_view(VirtualMachine): {e}")))?;
        let view = ContainerView::new(self.client.clone(), &view_ref.value);

        let morefs = view
            .view()
            .await
            .map_err(|e| Error::Vsphere(format!("ContainerView.view: {e}")))?
            .unwrap_or_default();
        let _ = view.destroy_view().await;

        // Filter to templates and match by name. vCenter inventories can have
        // thousands of VMs; we ask each per-VM rather than fetching all configs
        // up front because PropertyCollector batching is iteration-2b territory.
        // The common case (handful of templates per DC) is fine without it.
        for moref in morefs {
            let vmm = VimVirtualMachine::new(self.client.clone(), &moref.value);
            let cfg = match vmm.config().await {
                Ok(Some(c)) => c,
                Ok(None) => continue,
                Err(e) => {
                    return Err(Error::Vsphere(format!(
                        "VirtualMachine.config({}): {e}",
                        moref.value
                    )));
                }
            };
            if !cfg.template {
                continue;
            }
            if cfg.name != name {
                continue;
            }
            return Ok(Some(Template {
                name: cfg.name,
                moref: moref.value,
                datacenter_moref: dc.moref.clone(),
            }));
        }
        Ok(None)
    }
}

#[cfg(test)]
#[path = "vim_tests.rs"]
mod vim_tests;
