// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::reconciler`].

#[cfg(test)]
mod tests {
    use super::super::*;

    // The Action type is opaque (Debug only, no Eq), so we assert that the
    // constructors are callable and produce different values. This guards
    // against accidental swapping of the constants without much ceremony.
    #[test]
    fn helpers_are_callable_and_distinct_debug() {
        let default = format!("{:?}", requeue_default());
        let error = format!("{:?}", requeue_on_error());
        let long = format!("{:?}", requeue_long());
        let none = format!("{:?}", no_requeue());

        assert_ne!(default, error);
        assert_ne!(default, long);
        assert_ne!(default, none);
        assert_ne!(error, none);
    }

    #[test]
    fn requeue_intervals_are_monotonic() {
        const _: () = assert!(REQUEUE_ON_ERROR_SECS < REQUEUE_DEFAULT_SECS);
        const _: () = assert!(REQUEUE_DEFAULT_SECS < REQUEUE_LONG_SECS);
    }

    // --- serves --------------------------------------------------------------

    #[test]
    fn a_provider_serves_only_its_own_machines() {
        assert!(serves(Some("slate-kvm"), "slate-kvm"));
        assert!(!serves(Some("grill-kvm"), "slate-kvm"));
    }

    /// A statically installed provider has no `--provider-name` and serves
    /// every Provider of its class.
    #[test]
    fn a_provider_without_a_name_serves_every_machine() {
        assert!(serves(None, "slate-kvm"));
        assert!(serves(None, "grill-kvm"));
    }
}
