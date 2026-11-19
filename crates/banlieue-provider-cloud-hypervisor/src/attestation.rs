// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! The per-guest certificate signer a host offers once it opts in
//! (ADR-0146 Decision 2, ADR-0143 Decisions 3 to 5).
//!
//! A guest connecting to host vsock port [`ATTESTATION_PORT`] reaches the
//! Unix socket `<run dir>/vsock.sock_1025`, where the provider runs
//! [`banlieue_attestation_ca::serve`] for that guest alone. The CA and the
//! request loop live in `banlieue-attestation-ca`; this module decides
//! *when* to sign and *for whom*: the certificate subject is always the
//! machine's `metadata.uid`, fixed here and never read from a request.
//!
//! The CA is loaded once, at provider start ([`load_signer`]); a missing,
//! insecure or foreign-owned CA stops the provider with a message saying what
//! to fix. Nothing here logs a CSR or certificate: only the signer's numeric
//! counters are surfaced.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::sync::{Arc, Mutex};

use banlieue_attestation_ca::{
    AttestationCa, CaError, OutcomeCounts, Outcomes, RateLimit, ServeConfig, Signer,
    SignerConfigError, serve,
};
use thiserror::Error;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::host_config::AttestationSection;
use crate::plan::MachinePlan;

/// The CA's private key inside the CA directory (the crate's layout).
const CA_KEY_FILE: &str = "ca.key";
/// What to run to create the CA the host config points at.
const INSTALL_HINT: &str = "run `banlieue host cloud-hypervisor install` with attestation enabled";

/// Why the provider cannot start its signer.
#[derive(Debug, Error)]
pub enum SetupError {
    /// The CA could not be loaded (missing, insecure, corrupt).
    #[error(
        "attestation CA in {dir}: {source}; {INSTALL_HINT}, or fix the directory's modes (0700) and ca.key (0600)"
    )]
    Ca {
        /// The configured `ca_dir`.
        dir: String,
        /// Why.
        source: CaError,
    },
    /// `ca.key` is not owned by the provider's user.
    #[error(
        "{path} must be owned by the provider's user (uid {expected}) but is owned by uid {actual:?}; `chown` it, or recreate the CA as that user"
    )]
    NotOwned {
        /// The key file.
        path: String,
        /// The provider's uid.
        expected: u32,
        /// The file's owner, when it could be read.
        actual: Option<u32>,
    },
    /// The signer's own settings are unacceptable.
    #[error("attestation signer: {0}")]
    Signer(#[from] SignerConfigError),
}

/// Refuse a CA key that `uid` does not own: the crate checks modes, and a
/// `0600` key owned by someone else is still theirs to replace.
///
/// # Errors
/// [`SetupError::NotOwned`] when the key is missing, a symlink, or owned by
/// another uid.
pub fn check_key_owner(ca_dir: &Path, uid: u32) -> Result<(), SetupError> {
    let path = ca_dir.join(CA_KEY_FILE);
    let actual = fs::symlink_metadata(&path)
        .ok()
        .filter(|m| m.file_type().is_file())
        .map(|m| m.uid());
    if actual == Some(uid) {
        return Ok(());
    }
    Err(SetupError::NotOwned {
        path: path.display().to_string(),
        expected: uid,
        actual,
    })
}

/// Load the host's CA and build the signer every guest shares: the rate
/// limit is per subject, so one guest cannot exhaust another's.
///
/// # Errors
/// [`SetupError`] naming what to fix; the provider must not start on one.
pub fn load_signer(section: &AttestationSection, uid: u32) -> Result<Arc<Signer>, SetupError> {
    let ca = AttestationCa::load(&section.ca_dir).map_err(|source| SetupError::Ca {
        dir: section.ca_dir.display().to_string(),
        source,
    })?;
    check_key_owner(&section.ca_dir, uid)?;
    Ok(Arc::new(Signer::new(
        ca,
        section.certificate_ttl(),
        RateLimit::default(),
    )?))
}

/// The certificate subject for `plan`'s guest: the machine's UID, only.
#[must_use]
pub fn signer_subject(plan: &MachinePlan) -> &str {
    &plan.uid
}

struct Entry {
    task: JoinHandle<()>,
    cancel: CancellationToken,
    outcomes: Arc<Outcomes>,
}

/// One signer per guest, keyed by the subject (the machine UID).
pub struct Signers {
    signer: Arc<Signer>,
    inner: Mutex<HashMap<String, Entry>>,
    started: Mutex<HashMap<String, u32>>,
}

impl Signers {
    /// Signers sharing `signer` (and so its CA and rate limit).
    #[must_use]
    pub fn new(signer: Arc<Signer>) -> Self {
        Self {
            signer,
            inner: Mutex::default(),
            started: Mutex::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Serve certificate requests for `subject` at `path`, unless a signer
    /// is already running there. The socket is handed to `guest_uid`:`group`
    /// ([`crate::report`]'s handover, mode `0660`). A dead signer, or a
    /// socket that went missing, is started again; a stale file is replaced.
    ///
    /// # Errors
    /// Binding, or a socket that is not the one just bound.
    pub async fn ensure(
        &self,
        subject: &str,
        path: &Path,
        guest_uid: u32,
        group: u32,
    ) -> io::Result<()> {
        let alive = self
            .lock()
            .get(subject)
            .and_then(|e| (!e.task.is_finished()).then(|| e.outcomes.snapshot()));
        if let Some(counts) = alive
            && fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket())
        {
            debug!(machine = %subject, ?counts, "attestation signer running");
            return Ok(());
        }
        // Whatever is there (a stale socket, or something the guest put
        // there) is replaced: unlink removes a symlink, never its target.
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(path)?;
        crate::report::hand_to_guest(path, guest_uid, group)?;

        let cancel = CancellationToken::new();
        let outcomes = Arc::new(Outcomes::new());
        let task = tokio::spawn(serve(
            listener,
            self.signer.clone(),
            subject.to_string(),
            outcomes.clone(),
            ServeConfig::default(),
            cancel.clone(),
        ));
        let old = self.lock().insert(
            subject.to_string(),
            Entry {
                task,
                cancel,
                outcomes,
            },
        );
        if let Some(old) = old {
            old.cancel.cancel();
        }
        *self
            .started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(subject.to_string())
            .or_default() += 1;
        info!(machine = %subject, "attestation signer started");
        Ok(())
    }

    /// Stop `subject`'s signer and remove its socket. Not running is fine.
    pub fn stop(&self, subject: &str, path: &Path) {
        if let Some(e) = self.lock().remove(subject) {
            e.cancel.cancel();
            debug!(machine = %subject, counts = ?e.outcomes.snapshot(), "attestation signer stopped");
        }
        let _ = fs::remove_file(path);
    }

    /// What `subject`'s signer has done so far: numbers, never content.
    #[must_use]
    pub fn counts(&self, subject: &str) -> Option<OutcomeCounts> {
        self.lock().get(subject).map(|e| e.outcomes.snapshot())
    }

    /// Whether `subject` has a signer that is still serving.
    #[must_use]
    pub fn is_serving(&self, subject: &str) -> bool {
        self.lock()
            .get(subject)
            .is_some_and(|e| !e.task.is_finished())
    }

    /// How many times a signer was started for `subject` since this process
    /// began (more than one means a restart).
    #[must_use]
    pub fn starts(&self, subject: &str) -> u32 {
        self.started
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(subject)
            .copied()
            .unwrap_or(0)
    }

    /// End `subject`'s serve task without a stop, as a crash would.
    #[cfg(test)]
    pub(crate) async fn kill_for_test(&self, subject: &str) {
        let task = self.lock().remove(subject).map(|e| {
            e.cancel.cancel();
            e.task
        });
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

/// Start `plan`'s signer, when its host opted in.
///
/// # Errors
/// An I/O error from [`Signers::ensure`], or `Other` when the plan has a
/// signer socket but this provider loaded no CA (host config and process
/// disagree).
pub async fn ensure_for_plan(
    signers: Option<&Signers>,
    plan: &MachinePlan,
    group: u32,
) -> io::Result<()> {
    let Some(path) = &plan.attestation_socket else {
        return Ok(());
    };
    let Some(signers) = signers else {
        return Err(io::Error::other(
            "the host config has [attestation] but this provider holds no CA signer",
        ));
    };
    signers
        .ensure(signer_subject(plan), path, plan.host_uid, group)
        .await
}

/// Stop `plan`'s signer, if it has one.
pub fn stop_for_plan(signers: Option<&Signers>, plan: &MachinePlan) {
    let (Some(signers), Some(path)) = (signers, &plan.attestation_socket) else {
        return;
    };
    signers.stop(signer_subject(plan), path);
}

#[cfg(test)]
#[path = "attestation_tests.rs"]
mod attestation_tests;
