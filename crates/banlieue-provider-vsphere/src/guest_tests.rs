// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `guest.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::client::{FakeClient, Inventory};
    use banlieue_api::common::PowerState;

    fn fake() -> FakeClient {
        FakeClient::new(Inventory::default())
    }

    fn as_client(c: &FakeClient) -> &dyn crate::client::VSphereClient {
        c
    }

    // ------------------------------------------------------------------
    // classify (via probe_guest — private, exercised through the public API)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn probe_guest_is_not_evaluated_when_vm_is_powered_off() {
        let client = fake();
        client.set_guest_info("vm-1", PHASE_KEY, PHASE_INSTALLED);
        let probe = probe_guest(as_client(&client), "vm-1", PowerState::PoweredOff)
            .await
            .unwrap();
        assert_eq!(probe, GuestProbe::NotEvaluated);
    }

    #[tokio::test]
    async fn probe_guest_is_not_announced_when_key_absent() {
        let client = fake();
        let probe = probe_guest(as_client(&client), "vm-1", PowerState::PoweredOn)
            .await
            .unwrap();
        assert_eq!(probe, GuestProbe::NotAnnounced);
    }

    #[tokio::test]
    async fn probe_guest_is_not_announced_for_a_mismatched_value() {
        let client = fake();
        client.set_guest_info("vm-1", PHASE_KEY, "installing");
        let probe = probe_guest(as_client(&client), "vm-1", PowerState::PoweredOn)
            .await
            .unwrap();
        assert_eq!(probe, GuestProbe::NotAnnounced);
    }

    #[tokio::test]
    async fn probe_guest_is_installed_on_exact_match() {
        let client = fake();
        client.set_guest_info("vm-1", PHASE_KEY, PHASE_INSTALLED);
        let probe = probe_guest(as_client(&client), "vm-1", PowerState::PoweredOn)
            .await
            .unwrap();
        assert_eq!(probe, GuestProbe::Installed);
    }

    #[tokio::test]
    async fn probe_guest_trims_surrounding_whitespace() {
        let client = fake();
        client.set_guest_info("vm-1", PHASE_KEY, "  installed\n");
        let probe = probe_guest(as_client(&client), "vm-1", PowerState::PoweredOn)
            .await
            .unwrap();
        assert_eq!(probe, GuestProbe::Installed);
    }

    #[tokio::test]
    async fn probe_guest_only_reads_the_named_vm() {
        let client = fake();
        client.set_guest_info("vm-other", PHASE_KEY, PHASE_INSTALLED);
        let probe = probe_guest(as_client(&client), "vm-1", PowerState::PoweredOn)
            .await
            .unwrap();
        assert_eq!(probe, GuestProbe::NotAnnounced);
    }

    // ------------------------------------------------------------------
    // sticky_guest_installed (ADR-0043 Decision 5)
    // ------------------------------------------------------------------

    #[test]
    fn sticky_guest_installed_starts_at_whatever_was_observed() {
        assert_eq!(sticky_guest_installed(None, false), Some(false));
        assert_eq!(sticky_guest_installed(None, true), Some(true));
    }

    #[test]
    fn sticky_guest_installed_never_reverts_once_true() {
        assert_eq!(sticky_guest_installed(Some(true), false), Some(true));
        assert_eq!(sticky_guest_installed(Some(true), true), Some(true));
    }

    #[test]
    fn sticky_guest_installed_tracks_the_latest_observation_until_true() {
        assert_eq!(sticky_guest_installed(Some(false), true), Some(true));
        assert_eq!(sticky_guest_installed(Some(false), false), Some(false));
    }

    // ------------------------------------------------------------------
    // should_poll_soon (ADR-0043 Decision 8)
    // ------------------------------------------------------------------

    #[test]
    fn should_poll_soon_only_while_a_running_guest_has_not_announced() {
        assert!(should_poll_soon(GuestProbe::NotAnnounced));
        assert!(!should_poll_soon(GuestProbe::Installed));
        assert!(!should_poll_soon(GuestProbe::NotEvaluated));
    }

    #[test]
    fn is_installed_is_true_only_for_the_installed_variant() {
        assert!(GuestProbe::Installed.is_installed());
        assert!(!GuestProbe::NotAnnounced.is_installed());
        assert!(!GuestProbe::NotEvaluated.is_installed());
    }
}
