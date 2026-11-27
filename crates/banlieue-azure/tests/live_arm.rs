// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Live protocol tier for `banlieue-azure` (ADR-0164; `rules/testing.md`).
//!
//! Proves that Azure Resource Manager and Entra ID accept what the client
//! sends and that the client reads what they answer. Every test is
//! `#[ignore]`d: running one is an explicit request to talk to Azure, so a
//! missing variable or a refused call is a **failure**, never a skip.
//!
//! The token comes the way production gets it: a ServiceAccount token
//! (audience `api://AzureADTokenExchange`) exchanged at Entra ID through a
//! federated credential (ADR-0159). Grant the identity only the roles the
//! provider would hold, so a missing RBAC action fails here, not in a
//! cluster.
//!
//! ```sh
//! kubectl create token <provider-sa> -n <ns> \
//!   --audience api://AzureADTokenExchange --duration 1h > "$TOKEN_FILE"
//! AZURE_SUBSCRIPTION_ID=00000000-0000-0000-0000-000000000000 \
//! AZURE_TENANT_ID=00000000-0000-0000-0000-000000000000 \
//! AZURE_CLIENT_ID=00000000-0000-0000-0000-000000000000 \
//! AZURE_FEDERATED_TOKEN_FILE="$TOKEN_FILE" \
//! AZURE_RESOURCE_GROUP=rg-banlieue-example AZURE_LOCATION=eastus2 \
//!   cargo test -p banlieue-azure --test live_arm -- --ignored --test-threads=1
//! ```
//!
//! **Without a cluster issuer** (a first check from a workstation), set
//! `AZURE_ACCESS_TOKEN_FILE` to a file holding an ARM access token instead of
//! the four workload identity variables, for example
//! `az account get-access-token --resource https://management.azure.com
//! --query accessToken -o tsv > "$TOKEN_FILE"`. Every ARM test then runs as
//! *that principal*, which usually holds far more than the provider's custom
//! role, so a missing RBAC action will not fail here, and the token exchange
//! test is not exercised. It is a smoke test of the client against a real
//! subscription, not a substitute for the federated run.
//!
//! **With OpenBao as the signer** (ADR-0176), the OpenBao test
//! (`the_openbao_signed_assertion_is_exchanged_for_an_arm_token`) needs
//! `AZURE_TENANT_ID`, `AZURE_CLIENT_ID` and the `BANLIEUE_OPENBAO_*`
//! variables instead of a token file: the source logs in with the
//! ServiceAccount token named by `BANLIEUE_OPENBAO_SA_TOKEN_FILE` and reads the
//! assertion from `identity/oidc/token/<role>`.
//!
//! The lifecycle test also needs `AZURE_SUBNET_ID`, `AZURE_IMAGE_VERSION_ID`
//! and `AZURE_SSH_PUBLIC_KEY` (optionally `AZURE_VM_SIZE`, `AZURE_ZONE`,
//! `AZURE_OS_DISK_GB`). It creates and deletes one VM (a few minutes and a
//! few cents) and fails if anything it tagged is left behind. In a
//! policy-governed subscription (ADR-0172), `AZURE_DISK_ENCRYPTION_SET_ID`
//! encrypts the OS disk with that set and `AZURE_ENCRYPTION_AT_HOST=true`
//! sets `securityProfile.encryptionAtHost`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use banlieue_azure::api::ArmApi;
use banlieue_azure::operation::{OperationHandle, OperationStatus};
use banlieue_azure::token::{AccessToken, parse_token_response};
use banlieue_azure::types::{Provisioning, vcpu_headroom};
use banlieue_azure::vm::{DEFAULT_ADMIN_USERNAME, nic_name, vm_name};
use banlieue_azure::{
    ArmClient, ArmConfig, FederatedConfig, FederatedToken, NicRequest, OwnershipTags,
    TokenProvider, VmRequest,
};
use banlieue_azure::{OpenBaoAssertion, OpenBaoConfig};

const DEFAULT_VM_SIZE: &str = "Standard_D2s_v7";
const DEFAULT_ZONE: &str = "1";
const DEFAULT_OS_DISK_GB: u32 = 32;
const OS_DISK_SKU: &str = "StandardSSD_LRS";
/// The spike's slowest create to `Succeeded` was 330 s.
const OPERATION_TIMEOUT: Duration = Duration::from_secs(900);
const POLL_FLOOR: Duration = Duration::from_secs(5);
const LIVE_KIND: &str = "AzureVirtualMachine";
const LIVE_NAMESPACE: &str = "banlieue-live";
const LIVE_PROVIDER: &str = "live-arm";

fn env(name: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| panic!("set {name}; see this file's module docs"))
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn install_provider() {
    let _ = banlieue_tls::install_default_crypto_provider();
}

fn token_source() -> Arc<FederatedToken> {
    install_provider();
    let config = FederatedConfig::from_lookup(|k| std::env::var(k).ok())
        .expect("workload identity variables");
    Arc::new(FederatedToken::new(config).expect("token source"))
}

/// Lifetime claimed for a workstation token: ARM tokens last at least an
/// hour, and the test run is far shorter.
const STATIC_TOKEN_LIFETIME_SECS: u64 = 3000;

/// A bearer token read once from `AZURE_ACCESS_TOKEN_FILE`; see the module docs
/// for why this is a smoke test and not the federated run.
struct StaticToken(String);

#[async_trait]
impl TokenProvider for StaticToken {
    async fn token(&self) -> banlieue_azure::Result<AccessToken> {
        let body = serde_json::json!({
            "access_token": self.0,
            "expires_in": STATIC_TOKEN_LIFETIME_SECS,
        });
        parse_token_response(body.to_string().as_bytes(), Instant::now())
    }
}

fn arm_token() -> Arc<dyn TokenProvider> {
    let Some(path) = std::env::var("AZURE_ACCESS_TOKEN_FILE")
        .ok()
        .filter(|v| !v.trim().is_empty())
    else {
        return token_source();
    };
    let token = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("AZURE_ACCESS_TOKEN_FILE {path}: {e}"));
    Arc::new(StaticToken(token.trim().to_string()))
}

fn client() -> ArmClient {
    install_provider();
    ArmClient::new(ArmConfig::new(&env("AZURE_SUBSCRIPTION_ID")), arm_token()).expect("client")
}

async fn wait(client: &ArmClient, handle: Option<OperationHandle>, what: &str) {
    let Some(handle) = handle else {
        return;
    };
    let deadline = Instant::now() + OPERATION_TIMEOUT;
    loop {
        match client.operation_status(&handle).await {
            Ok(OperationStatus::Succeeded) => return,
            Ok(OperationStatus::InProgress) => {}
            Ok(other) => panic!("{what}: {other:?}"),
            Err(e) if e.is_retryable() => {}
            Err(e) => panic!("{what}: {e}"),
        }
        assert!(Instant::now() < deadline, "{what}: still running");
        tokio::time::sleep(handle.retry_after.unwrap_or(POLL_FLOOR).max(POLL_FLOOR)).await;
    }
}

#[tokio::test]
#[ignore = "talks to Entra ID; see module docs"]
async fn the_service_account_token_is_exchanged_for_an_arm_token() {
    let source = token_source();
    let token = source.token().await.expect("exchange");
    assert!(!token.secret().is_empty());
    // Cached: the second call makes no exchange and returns the same token.
    let again = source.token().await.expect("cached");
    assert_eq!(again.expires_at(), token.expires_at());
}

#[tokio::test]
#[ignore = "talks to OpenBao and Entra ID; see module docs (ADR-0176)"]
async fn the_openbao_signed_assertion_is_exchanged_for_an_arm_token() {
    install_provider();
    let federated = FederatedConfig::from_lookup(|k| {
        // The assertion comes from OpenBao, so no token file is needed.
        (k == "AZURE_FEDERATED_TOKEN_FILE")
            .then(|| "/unused".to_string())
            .or_else(|| std::env::var(k).ok())
    })
    .expect("tenant and client variables");
    let openbao = OpenBaoAssertion::new(
        OpenBaoConfig::from_lookup(|k| std::env::var(k).ok())
            .expect("BANLIEUE_OPENBAO_* variables"),
    )
    .expect("openbao source");
    let source = FederatedToken::with_source(federated, Arc::new(openbao)).expect("token source");

    let token = source.token().await.expect("exchange");
    assert!(!token.secret().is_empty());
    // Cached: the second call reaches neither OpenBao nor Entra ID.
    let again = source.token().await.expect("cached");
    assert_eq!(again.expires_at(), token.expires_at());
}

#[tokio::test]
#[ignore = "talks to ARM; needs Microsoft.Resources/subscriptions/resourceGroups/resources/read"]
async fn the_resource_group_lists_with_tags_and_creation_times() {
    let resources = client()
        .list_resources(&env("AZURE_RESOURCE_GROUP"))
        .await
        .expect("list");
    // Every resource ARM returns has an ID, a type, and (with
    // $expand=createdTime) a creation time the orphan sweep can use.
    for r in &resources {
        assert!(!r.id.is_empty() && !r.resource_type.is_empty(), "{r:?}");
        assert!(r.created_time.is_some(), "no createdTime for {}", r.id);
    }
}

#[tokio::test]
#[ignore = "talks to ARM; needs Microsoft.Compute/virtualMachines/read"]
async fn a_missing_vm_is_not_found() {
    let err = client()
        .vm_instance_view(&env("AZURE_RESOURCE_GROUP"), "banlieue-live-no-such-vm")
        .await
        .unwrap_err();
    assert!(err.is_not_found(), "{err}");
}

#[tokio::test]
#[ignore = "talks to ARM; needs Microsoft.Compute/skus/read at subscription scope"]
async fn sizes_and_their_usable_zones_are_read() {
    let location = env("AZURE_LOCATION");
    let skus = client().resource_skus(&location).await.expect("skus");
    assert!(!skus.is_empty(), "no VM sizes in {location}");
    assert!(
        skus.iter().any(|s| !s.usable_zones().is_empty()),
        "no size has a usable zone in {location}"
    );
}

#[tokio::test]
#[ignore = "talks to ARM; needs Microsoft.Compute/locations/usages/read at subscription scope"]
async fn regional_quota_is_read() {
    let usages = client()
        .usages(&env("AZURE_LOCATION"))
        .await
        .expect("usages");
    assert!(
        usages.iter().any(|u| u.name == "cores"),
        "no regional `cores` usage"
    );
    assert!(vcpu_headroom(&usages, "StandardDsv7Family").is_some());
}

#[tokio::test]
#[ignore = "creates and deletes a VM; needs the provider's resource group role"]
async fn a_vm_is_created_boots_and_is_deleted_with_its_nic_and_disk() {
    let c = client();
    let rg = env("AZURE_RESOURCE_GROUP");
    let location = env("AZURE_LOCATION");
    let uid = format!("live-{}", jiff::Timestamp::now().as_second());
    let tags = OwnershipTags::new(&uid, LIVE_KIND, LIVE_NAMESPACE, LIVE_PROVIDER).to_map();
    let (vm, nic) = (vm_name(&uid), nic_name(&uid));

    // Deleting what does not exist is success, as ARM's 204 says.
    assert!(
        c.delete_vm(&rg, &vm)
            .await
            .expect("delete missing")
            .is_none()
    );

    let op = c
        .put_nic(
            &rg,
            &nic,
            &NicRequest {
                location: location.clone(),
                subnet_id: env("AZURE_SUBNET_ID"),
                accelerated_networking: true,
                tags: tags.clone(),
            },
        )
        .await
        .expect("put nic");
    wait(&c, op, "nic").await;
    let nic_id = format!(
        "/subscriptions/{}/resourceGroups/{rg}/providers/Microsoft.Network/networkInterfaces/{nic}",
        env("AZURE_SUBSCRIPTION_ID")
    );

    let request = VmRequest {
        location,
        zone: env_or("AZURE_ZONE", DEFAULT_ZONE),
        size: env_or("AZURE_VM_SIZE", DEFAULT_VM_SIZE),
        image_version_id: env("AZURE_IMAGE_VERSION_ID"),
        os_disk_size_gb: env_or("AZURE_OS_DISK_GB", &DEFAULT_OS_DISK_GB.to_string())
            .parse()
            .expect("AZURE_OS_DISK_GB"),
        os_disk_sku: OS_DISK_SKU.to_string(),
        os_disk_name: format!("{vm}-osdisk"),
        nic_id,
        computer_name: vm.clone(),
        admin_username: DEFAULT_ADMIN_USERNAME.to_string(),
        ssh_public_key: env("AZURE_SSH_PUBLIC_KEY"),
        custom_data: b"#cloud-config\n".to_vec(),
        trusted_launch: true,
        tags,
        disk_encryption_set_id: std::env::var("AZURE_DISK_ENCRYPTION_SET_ID")
            .ok()
            .filter(|id| !id.is_empty()),
        encryption_at_host: std::env::var("AZURE_ENCRYPTION_AT_HOST").is_ok_and(|v| v == "true"),
    };
    let created = c.put_vm(&rg, &vm, &request).await;
    if let Err(e) = &created {
        // Leave nothing behind even when the create is refused.
        let _ = c.delete_nic(&rg, &nic).await;
        panic!("put vm: {e}");
    }
    wait(&c, created.expect("put vm"), "vm create").await;

    let view = c.vm_instance_view(&rg, &vm).await.expect("instance view");
    assert_eq!(view.provisioning, Provisioning::Succeeded, "{view:?}");
    assert!(c.nic_private_ip(&rg, &nic).await.expect("nic").is_some());

    let op = c.delete_vm(&rg, &vm).await.expect("delete vm");
    wait(&c, op, "vm delete").await;

    // deleteOption: Delete took the NIC and the OS disk.
    let left: Vec<_> = c
        .list_resources(&rg)
        .await
        .expect("list")
        .into_iter()
        .filter(|r| r.tags.values().any(|v| v == &uid) || r.name.starts_with(&vm))
        .map(|r| r.id)
        .collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}
