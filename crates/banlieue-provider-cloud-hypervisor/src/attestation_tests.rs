// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `attestation.rs`: real Unix sockets and a real CA in a
//! temporary directory, so what is asserted is what a guest's helper would
//! see on its vsock.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::host_config::AttestationSection;
    use crate::plan::plan_machine;
    use crate::test_fixtures::{config, spec, with_attestation};
    use banlieue_attestation_ca::{AttestationCa, RateLimit};
    use p256::ecdsa::{DerSignature, SigningKey};
    use p256::elliptic_curve::Generate;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use std::str::FromStr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;
    use x509_cert::builder::{Builder, RequestBuilder};
    use x509_cert::der::{EncodePem, pem::LineEnding};
    use x509_cert::name::Name;

    const UID: &str = "0f3c9a1e-5b7d-4e2a-9c11-3f2b7a5d9e01";
    const HOST_UID: u32 = 2_000_000;
    const CA_NAME: &str = "host-01";
    const TTL_SECS: u64 = 3600;
    const SOCKET_FILE: &str = "vsock.sock_1025";

    fn csr() -> String {
        let key = SigningKey::generate();
        RequestBuilder::new(Name::from_str("CN=guest").unwrap())
            .unwrap()
            .build::<_, DerSignature>(&key)
            .unwrap()
            .to_pem(LineEnding::LF)
            .unwrap()
    }

    fn signer_in(dir: &std::path::Path) -> Arc<Signer> {
        let ca = AttestationCa::create(&dir.join("ca"), CA_NAME, None).unwrap();
        Arc::new(
            Signer::new(
                ca,
                std::time::Duration::from_secs(TTL_SECS),
                RateLimit::default(),
            )
            .unwrap(),
        )
    }

    fn uid_gid() -> (u32, u32) {
        (crate::sys::effective_uid(), crate::sys::effective_gid())
    }

    /// Ask the signer on `path` for a certificate.
    async fn ask(path: &std::path::Path, request: &str) -> String {
        let mut s = UnixStream::connect(path).await.unwrap();
        s.write_all(request.as_bytes()).await.unwrap();
        s.shutdown().await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        out
    }

    fn common_name(pem: &str) -> String {
        let (_, p) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).unwrap();
        let cert = p.parse_x509().unwrap();
        cert.subject().to_string()
    }

    // ------------------------------------------------------------------
    // Signers
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn a_guest_gets_a_certificate_whose_subject_is_the_machine_uid() {
        let tmp = tempfile::tempdir().unwrap();
        let signers = Signers::new(signer_in(tmp.path()));
        let path = tmp.path().join(SOCKET_FILE);
        let (uid, gid) = uid_gid();
        signers.ensure(UID, &path, uid, gid).await.unwrap();
        let pem = ask(&path, &csr()).await;
        assert_eq!(common_name(&pem), format!("CN={UID}"));
    }

    #[tokio::test]
    async fn the_socket_is_the_guests_and_closed_to_everyone_else() {
        let tmp = tempfile::tempdir().unwrap();
        let signers = Signers::new(signer_in(tmp.path()));
        let path = tmp.path().join(SOCKET_FILE);
        let (uid, gid) = uid_gid();
        signers.ensure(UID, &path, uid, gid).await.unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert!(meta.file_type().is_socket());
        assert_eq!((meta.uid(), meta.gid()), (uid, gid));
        assert_eq!(meta.permissions().mode() & 0o777, 0o660);
    }

    /// A reconcile pass runs every few seconds; it must not start a second
    /// serve for a guest that already has one.
    #[tokio::test]
    async fn a_second_pass_does_not_start_a_second_signer() {
        let tmp = tempfile::tempdir().unwrap();
        let signers = Signers::new(signer_in(tmp.path()));
        let path = tmp.path().join(SOCKET_FILE);
        let (uid, gid) = uid_gid();
        signers.ensure(UID, &path, uid, gid).await.unwrap();
        let first = std::fs::symlink_metadata(&path).unwrap().ino();
        assert_eq!(signers.starts(UID), 1);
        signers.ensure(UID, &path, uid, gid).await.unwrap();
        assert_eq!(std::fs::symlink_metadata(&path).unwrap().ino(), first);
        assert_eq!(signers.starts(UID), 1);
    }

    #[tokio::test]
    async fn a_signer_that_died_is_restarted() {
        let tmp = tempfile::tempdir().unwrap();
        let signers = Signers::new(signer_in(tmp.path()));
        let path = tmp.path().join(SOCKET_FILE);
        let (uid, gid) = uid_gid();
        signers.ensure(UID, &path, uid, gid).await.unwrap();
        signers.kill_for_test(UID).await;
        assert!(!signers.is_serving(UID));
        signers.ensure(UID, &path, uid, gid).await.unwrap();
        assert_eq!(signers.starts(UID), 2);
        assert!(common_name(&ask(&path, &csr()).await).contains(UID));
    }

    #[tokio::test]
    async fn a_socket_someone_removed_is_bound_again() {
        let tmp = tempfile::tempdir().unwrap();
        let signers = Signers::new(signer_in(tmp.path()));
        let path = tmp.path().join(SOCKET_FILE);
        let (uid, gid) = uid_gid();
        signers.ensure(UID, &path, uid, gid).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        signers.ensure(UID, &path, uid, gid).await.unwrap();
        assert!(common_name(&ask(&path, &csr()).await).contains(UID));
    }

    /// A crashed provider leaves its socket file behind; that is not an error.
    #[tokio::test]
    async fn a_stale_socket_from_a_crashed_provider_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(SOCKET_FILE);
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists(), "the stale socket file");
        let signers = Signers::new(signer_in(tmp.path()));
        let (uid, gid) = uid_gid();
        signers.ensure(UID, &path, uid, gid).await.unwrap();
        assert!(common_name(&ask(&path, &csr()).await).contains(UID));
    }

    /// Whatever a guest put at the path is replaced, never followed.
    #[tokio::test]
    async fn a_planted_symlink_is_replaced_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("victim");
        std::fs::write(&target, "keep").unwrap();
        let path = tmp.path().join(SOCKET_FILE);
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let signers = Signers::new(signer_in(tmp.path()));
        let (uid, gid) = uid_gid();
        signers.ensure(UID, &path, uid, gid).await.unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_socket()
        );
    }

    #[tokio::test]
    async fn stopping_ends_the_signer_and_removes_the_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let signers = Signers::new(signer_in(tmp.path()));
        let path = tmp.path().join(SOCKET_FILE);
        let (uid, gid) = uid_gid();
        signers.ensure(UID, &path, uid, gid).await.unwrap();
        signers.stop(UID, &path);
        assert!(!path.exists());
        assert!(!signers.is_serving(UID));
        assert!(signers.counts(UID).is_none());
        // Stopping what is not running is fine.
        signers.stop(UID, &path);
    }

    #[tokio::test]
    async fn each_guest_has_its_own_signer_and_subject() {
        let tmp = tempfile::tempdir().unwrap();
        let signers = Signers::new(signer_in(tmp.path()));
        let other = "1a2b3c4d-0000-4000-8000-000000000001";
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        let (uid, gid) = uid_gid();
        signers.ensure(UID, &a, uid, gid).await.unwrap();
        signers.ensure(other, &b, uid, gid).await.unwrap();
        assert!(common_name(&ask(&a, &csr()).await).contains(UID));
        assert!(common_name(&ask(&b, &csr()).await).contains(other));
        signers.stop(UID, &a);
        assert!(signers.is_serving(other), "stopping one leaves the other");
    }

    /// The counters say what happened, in numbers, with no request content.
    #[tokio::test]
    async fn the_counters_report_issued_and_refused_requests() {
        let tmp = tempfile::tempdir().unwrap();
        let signers = Signers::new(signer_in(tmp.path()));
        let path = tmp.path().join(SOCKET_FILE);
        let (uid, gid) = uid_gid();
        signers.ensure(UID, &path, uid, gid).await.unwrap();
        ask(&path, &csr()).await;
        ask(&path, "not a csr").await;
        // The server counts after replying; give it the chance to finish.
        let mut counts = signers.counts(UID).unwrap();
        for _ in 0..200 {
            if counts.issued == 1 && counts.malformed == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            counts = signers.counts(UID).unwrap();
        }
        assert_eq!((counts.issued, counts.malformed), (1, 1), "{counts:?}");
    }

    // ------------------------------------------------------------------
    // The plan seam
    // ------------------------------------------------------------------

    fn opted_in_plan(run: &std::path::Path) -> crate::plan::MachinePlan {
        let mut p = plan_machine(
            UID,
            "m1",
            "ch-a",
            &spec(),
            &with_attestation(config()),
            HOST_UID,
        )
        .unwrap();
        p.attestation_socket = Some(run.join(SOCKET_FILE));
        // The guest handover chowns to the plan's uid; an unprivileged test
        // may only hand a socket to itself.
        p.host_uid = crate::sys::effective_uid();
        p
    }

    #[test]
    fn the_subject_is_the_machines_uid_and_nothing_else() {
        let mut p = opted_in_plan(std::path::Path::new("/run/x"));
        p.host_uid = HOST_UID;
        assert_eq!(signer_subject(&p), UID);
        assert_ne!(signer_subject(&p), HOST_UID.to_string());
        assert_ne!(signer_subject(&p), "m1");
    }

    #[tokio::test]
    async fn a_certificate_for_a_plan_names_the_machine_uid_not_the_host_uid() {
        let tmp = tempfile::tempdir().unwrap();
        let signers = Signers::new(signer_in(tmp.path()));
        let plan = opted_in_plan(tmp.path());
        ensure_for_plan(Some(&signers), &plan, crate::sys::effective_gid())
            .await
            .unwrap();
        let pem = ask(plan.attestation_socket.as_ref().unwrap(), &csr()).await;
        assert_eq!(common_name(&pem), format!("CN={UID}"));
    }

    #[tokio::test]
    async fn a_plan_without_a_signer_socket_starts_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let signers = Signers::new(signer_in(tmp.path()));
        let plan = plan_machine(UID, "m1", "ch-a", &spec(), &config(), HOST_UID).unwrap();
        ensure_for_plan(Some(&signers), &plan, 0).await.unwrap();
        assert!(!signers.is_serving(UID));
        // Not even a provider with no signer at all objects.
        ensure_for_plan(None, &plan, 0).await.unwrap();
    }

    #[tokio::test]
    async fn an_opted_in_plan_on_a_provider_with_no_signer_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = opted_in_plan(tmp.path());
        let err = ensure_for_plan(None, &plan, 0).await.unwrap_err();
        assert!(err.to_string().contains("[attestation]"), "{err}");
    }

    #[tokio::test]
    async fn stopping_for_a_plan_removes_its_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let signers = Signers::new(signer_in(tmp.path()));
        let plan = opted_in_plan(tmp.path());
        ensure_for_plan(Some(&signers), &plan, crate::sys::effective_gid())
            .await
            .unwrap();
        stop_for_plan(Some(&signers), &plan);
        assert!(!plan.attestation_socket.as_ref().unwrap().exists());
        assert!(!signers.is_serving(UID));
        // And a plan that never opted in is left alone.
        stop_for_plan(None, &plan);
    }

    // ------------------------------------------------------------------
    // Startup: the CA must exist, be safe, and be the provider's alone
    // ------------------------------------------------------------------

    fn section(dir: &std::path::Path) -> AttestationSection {
        AttestationSection {
            ca_dir: dir.to_path_buf(),
            certificate_ttl_secs: TTL_SECS,
        }
    }

    #[test]
    fn a_ca_the_provider_owns_loads_and_signs_for_the_configured_ttl() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ca");
        AttestationCa::create(&dir, CA_NAME, None).unwrap();
        let signer = load_signer(&section(&dir), crate::sys::effective_uid()).unwrap();
        assert_eq!(signer.ca().common_name(), CA_NAME);
        let pem = signer
            .sign(csr().as_bytes(), UID, std::time::SystemTime::now())
            .unwrap();
        let (_, p) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).unwrap();
        let cert = p.parse_x509().unwrap();
        let v = cert.validity();
        let life = v.not_after.timestamp() - v.not_before.timestamp();
        assert_eq!(life, i64::try_from(TTL_SECS).unwrap());
    }

    #[test]
    fn a_missing_ca_is_fatal_and_says_what_to_fix() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nope");
        let err = load_signer(&section(&dir), crate::sys::effective_uid())
            .unwrap_err()
            .to_string();
        assert!(err.contains(&dir.display().to_string()), "{err}");
        assert!(
            err.contains("banlieue host cloud-hypervisor install"),
            "{err}"
        );
    }

    #[test]
    fn an_insecure_ca_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ca");
        AttestationCa::create(&dir, CA_NAME, None).unwrap();
        std::fs::set_permissions(dir.join("ca.key"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        let err = load_signer(&section(&dir), crate::sys::effective_uid()).unwrap_err();
        assert!(matches!(err, SetupError::Ca { .. }), "{err}");
        assert!(err.to_string().contains("0600"), "{err}");
    }

    /// The crate checks modes, not owners: a key another user owns could be
    /// replaced by that user, so the provider refuses to sign with it.
    #[test]
    fn a_key_owned_by_someone_else_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ca");
        AttestationCa::create(&dir, CA_NAME, None).unwrap();
        let me = crate::sys::effective_uid();
        let err = load_signer(&section(&dir), me.wrapping_add(1)).unwrap_err();
        assert!(matches!(err, SetupError::NotOwned { .. }), "{err}");
        let text = err.to_string();
        assert!(text.contains("ca.key") && text.contains("chown"), "{text}");
        assert!(check_key_owner(&dir, me).is_ok());
        assert!(check_key_owner(&dir, me.wrapping_add(1)).is_err());
    }

    #[test]
    fn a_ca_directory_without_a_key_has_no_owner_to_check() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(matches!(
            check_key_owner(tmp.path(), 0),
            Err(SetupError::NotOwned { .. } | SetupError::Ca { .. })
        ));
    }
}
