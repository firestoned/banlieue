// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Reading the installed-guest marker via vSphere `guestinfo` (ADR-0043).
//!
//! vSphere already has a host-readable, bidirectional guest channel:
//! `vmware-rpctool "info-set guestinfo.banlieue.phase installed"` inside the
//! guest makes the value show up in `config.extraConfig` on the host side,
//! with no agent protocol to negotiate the way libvirt's `qemu-guest-agent`
//! needs. The marker distinguishes a guest booted from its *installed* disk
//! from the live installer that is still overwriting it — see ADR-0043 for
//! why every cheaper signal (a Tools heartbeat, a DHCP lease, an open port)
//! answers the wrong question.

use banlieue_api::common::PowerState;

use crate::client::VSphereClient;
use crate::error::Result;

/// `extraConfig` key the installed guest writes.
pub const PHASE_KEY: &str = "guestinfo.banlieue.phase";

/// The only value that means "booted from the installed disk".
pub const PHASE_INSTALLED: &str = "installed";

/// What a guest observation found.
///
/// Two negative states, not one, mirroring `banlieue-provider-libvirt`'s own
/// `GuestProbe`: a stopped VM cannot be evaluated at all (nothing is running
/// to have written the marker this boot), while a running VM whose
/// `extraConfig` has no matching key may simply still be installing. Only
/// the second case should drive a fast requeue (ADR-0043 Decision 8) — a
/// stopped VM changes only when something else powers it back on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestProbe {
    /// The VM is not powered on, so its guest cannot be writing anything
    /// this boot.
    NotEvaluated,
    /// The VM is running, but `extraConfig` carries no `installed` marker
    /// yet.
    NotAnnounced,
    /// The marker says the installed system is running.
    Installed,
}

impl GuestProbe {
    /// Whether the installed guest has announced itself.
    #[must_use]
    pub fn is_installed(self) -> bool {
        self == Self::Installed
    }
}

/// Ask vCenter whether `vm_moref`'s installed guest has announced itself.
///
/// Reads `guestinfo.banlieue.phase` from `config.extraConfig` only while the
/// VM is powered on: the value lives in guest runtime state and does not
/// survive a power cycle (ADR-0043 Decision 3), so a stopped VM cannot be
/// telling us anything about *this* boot.
///
/// # Errors
/// Propagates a transport/authentication failure from
/// [`VSphereClient::guest_info`] — the same failure mode every other call on
/// this trait has. There is no guest-side error to swallow here, unlike
/// libvirt's agent probe: `config.extraConfig` is a plain VM property, not a
/// negotiated protocol that can be "unreachable" while the VM itself answers
/// fine.
pub async fn probe_guest(
    client: &dyn VSphereClient,
    vm_moref: &str,
    power_state: PowerState,
) -> Result<GuestProbe> {
    if power_state != PowerState::PoweredOn {
        return Ok(GuestProbe::NotEvaluated);
    }
    let value = client.guest_info(vm_moref, PHASE_KEY).await?;
    Ok(classify(value.as_deref()))
}

/// Classify a raw `extraConfig` value. Exact match after trimming — a
/// substring test would let `not-installed` read as installed.
fn classify(value: Option<&str>) -> GuestProbe {
    match value.map(str::trim) {
        Some(v) if v == PHASE_INSTALLED => GuestProbe::Installed,
        _ => GuestProbe::NotAnnounced,
    }
}

/// Fold a fresh observation into the stored `guestInstalled`, stickily.
///
/// Once `Some(true)`, it stays: the marker does not survive a power cycle,
/// but a VM that was stopped has not become uninstalled (ADR-0043
/// Decision 5). `None` means nothing has looked yet, the expected state for
/// the whole of a `Deferred` image's install.
#[must_use]
pub fn sticky_guest_installed(previous: Option<bool>, observed: bool) -> Option<bool> {
    if previous == Some(true) {
        return Some(true);
    }
    Some(observed)
}

/// Whether to requeue at the default interval rather than the long one.
///
/// Fast only while the answer is expected to change soon (ADR-0043
/// Decision 8): a running VM that has not announced yet, i.e. a `Deferred`
/// install in progress. A stopped VM (`NotEvaluated`) is not a reason to
/// poll fast — nothing about the guest changes until something else powers
/// it back on, and `refresh_power_state`'s own long interval already covers
/// noticing that.
#[must_use]
pub fn should_poll_soon(guest: GuestProbe) -> bool {
    guest == GuestProbe::NotAnnounced
}

#[cfg(test)]
#[path = "guest_tests.rs"]
mod guest_tests;
