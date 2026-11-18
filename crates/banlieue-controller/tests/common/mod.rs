// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Helpers shared by the live suites in this directory: clients (the
//! kubeconfig's own identity, or an impersonated one), the admission verdict
//! of a dry-run write, and a random suffix for scratch object names.

// Each test binary compiles this module in full but uses only the parts it
// needs; without this, every binary warns about the rest.
#![allow(dead_code)]

use kube::Client;

/// The group whose members bypass RBAC, so a dry-run write's verdict is
/// admission's alone.
pub const PRIVILEGED_GROUP: &str = "system:masters";

/// HTTP status of a write a `ValidatingAdmissionPolicy` refused.
const HTTP_UNPROCESSABLE: u16 = 422;
/// HTTP status of a write RBAC refused.
const HTTP_FORBIDDEN: u16 = 403;

/// A client with the kubeconfig's own identity.
pub async fn admin() -> Client {
    Client::try_default()
        .await
        .expect("no reachable cluster: set KUBECONFIG")
}

/// A client impersonating `user` as a member of `groups`. The kubeconfig's
/// identity must be allowed to impersonate (cluster-admin).
pub async fn impersonating(user: &str, groups: Vec<String>) -> Client {
    let mut config = kube::Config::infer()
        .await
        .expect("kubeconfig: set KUBECONFIG");
    config.auth_info.impersonate = Some(user.to_string());
    config.auth_info.impersonate_groups = Some(groups);
    Client::try_from(config).expect("impersonating client")
}

/// A client impersonating `user` in [`PRIVILEGED_GROUP`].
pub async fn as_master(user: &str) -> Client {
    impersonating(user, vec![PRIVILEGED_GROUP.to_string()]).await
}

/// Whether a write was admitted: `true` when it succeeded, `false` when it
/// was denied (422 or 403, logged with `what`). Any other error panics, so an
/// unreachable cluster never reads as a denial.
pub fn verdict<T>(r: Result<T, kube::Error>, what: &str) -> bool {
    match r {
        Ok(_) => true,
        Err(kube::Error::Api(e)) if e.code == HTTP_UNPROCESSABLE || e.code == HTTP_FORBIDDEN => {
            eprintln!("    {what}: denied: {}", e.message);
            false
        }
        Err(e) => panic!("{what}: unexpected error: {e}"),
    }
}

/// Eight random hex characters, to keep one run's scratch objects apart
/// from another's.
pub fn nonce() -> String {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).unwrap();
    b.iter().map(|x| format!("{x:02x}")).collect()
}
