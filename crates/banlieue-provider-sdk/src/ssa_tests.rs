// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for field-manager naming (ADR-0087).
//!
//! These cover the *naming* half: that two writers of one provider class get
//! two distinct manager strings, and that the string stays inside the
//! apiserver's limit.
//!
//! They cannot cover the half that actually regressed. Whether two writers
//! erase each other's `perProvider` rows is a property of the apiserver's
//! server-side-apply merge, not of any Rust value, so it lives in
//! `banlieue-provider-libvirt/tests/e2e_vmimage_ssa.rs` against a real
//! cluster. A green run here means the names are right, not that ownership
//! works.

#[cfg(test)]
mod tests {
    use super::super::*;

    // ========================================================================
    // provider_field_manager: one writer, one name
    // ========================================================================

    /// The whole point of ADR-0087: two hosts of the same provider class must
    /// not share a manager name. Sharing one is what let each host's apply
    /// delete the other's row, forever.
    #[test]
    fn two_providers_of_one_class_get_distinct_managers() {
        let a = provider_field_manager(
            FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR,
            "banlieue-system",
            "host-a",
        );
        let b = provider_field_manager(
            FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR,
            "banlieue-system",
            "host-b",
        );
        assert_ne!(a, b, "two writers of one class must not share a manager");
    }

    /// Same name in two namespaces is two different `Provider` objects, so two
    /// different writers.
    #[test]
    fn same_provider_name_in_two_namespaces_gets_distinct_managers() {
        let a = provider_field_manager(FIELD_MANAGER_PROVIDER_LIBVIRT, "ns-a", "kvm-1");
        let b = provider_field_manager(FIELD_MANAGER_PROVIDER_LIBVIRT, "ns-b", "kvm-1");
        assert_ne!(a, b);
    }

    /// Two classes on one host stay distinct, which they already were. Pinned
    /// so a future "simplification" of the scheme cannot collapse them.
    #[test]
    fn two_classes_on_one_host_get_distinct_managers() {
        let libvirt = provider_field_manager(FIELD_MANAGER_PROVIDER_LIBVIRT, "ns", "host-a");
        let ch = provider_field_manager(FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR, "ns", "host-a");
        assert_ne!(libvirt, ch);
    }

    #[test]
    fn the_manager_is_deterministic() {
        let once = provider_field_manager(FIELD_MANAGER_PROVIDER_LIBVIRT, "ns", "kvm-1");
        let twice = provider_field_manager(FIELD_MANAGER_PROVIDER_LIBVIRT, "ns", "kvm-1");
        assert_eq!(
            once, twice,
            "a non-deterministic manager would orphan the rows of every restart"
        );
    }

    /// The name must keep the class readable, because `managedFields` is what
    /// an operator reads to answer "who wrote this row".
    #[test]
    fn the_manager_keeps_the_class_and_the_provider_identity_readable() {
        let manager = provider_field_manager(
            FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR,
            "banlieue-system",
            "host-a",
        );
        assert!(
            manager.starts_with(FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR),
            "must remain greppable by class, got {manager}"
        );
        assert!(
            manager.contains("host-a"),
            "must name the Provider, got {manager}"
        );
        assert!(
            manager.contains("banlieue-system"),
            "must name the namespace, got {manager}"
        );
    }

    // ========================================================================
    // The apiserver's 128-character cap
    // ========================================================================

    /// `metadata.managedFields[].manager` is capped at 128 characters by the
    /// apiserver. Exceeding it makes the apply fail outright, so a long
    /// namespace/name must truncate rather than produce a rejected patch.
    #[test]
    fn a_long_identity_is_truncated_to_the_apiserver_limit() {
        let manager = provider_field_manager(
            FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR,
            &"n".repeat(63),
            &"p".repeat(63),
        );
        assert!(
            manager.len() <= FIELD_MANAGER_MAX_LEN,
            "manager is {} chars, over the {FIELD_MANAGER_MAX_LEN} cap: {manager}",
            manager.len()
        );
    }

    /// Truncation must not reintroduce the bug: two different long identities
    /// that share a prefix must still differ, which is why the suffix is a
    /// hash rather than a cut.
    #[test]
    fn truncation_still_distinguishes_two_long_identities() {
        let long_ns = "n".repeat(63);
        let a = provider_field_manager(
            FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR,
            &long_ns,
            &format!("{}a", "p".repeat(62)),
        );
        let b = provider_field_manager(
            FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR,
            &long_ns,
            &format!("{}b", "p".repeat(62)),
        );
        assert_ne!(
            a, b,
            "truncation must not collapse two writers back into one manager"
        );
        assert!(a.len() <= FIELD_MANAGER_MAX_LEN && b.len() <= FIELD_MANAGER_MAX_LEN);
    }

    #[test]
    fn a_short_identity_is_not_truncated() {
        let manager =
            provider_field_manager(FIELD_MANAGER_PROVIDER_LIBVIRT, "banlieue-system", "kvm-1");
        assert_eq!(
            manager,
            format!("{FIELD_MANAGER_PROVIDER_LIBVIRT}/banlieue-system/kvm-1"),
            "the common case should read plainly, with no hash"
        );
    }

    // ========================================================================
    // Which managers are allowed to stay class-scoped
    // ========================================================================

    /// ADR-0087 decision 1: scoping is required exactly when a writer applies
    /// a *subset* of what the manager could own, not whenever several
    /// processes run.
    ///
    /// vSphere, Proxmox and libvirt each apply **every** `perProvider` row of
    /// their class in one patch, so several replicas under one manager are
    /// idempotent and the class constant is correct. Cloud Hypervisor is
    /// host-resident (ADR-0060) and applies **only its own** row, so it must
    /// scope. The constants are not interchangeable and this pins which is
    /// which.
    #[test]
    fn scoping_a_manager_always_changes_it() {
        for class in [
            FIELD_MANAGER_PROVIDER_VSPHERE,
            FIELD_MANAGER_PROVIDER_PROXMOX,
            FIELD_MANAGER_PROVIDER_LIBVIRT,
            FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR,
        ] {
            let scoped = provider_field_manager(class, "ns", "p");
            assert_ne!(
                scoped, class,
                "scoping {class} must produce a different manager, or a \
                 single-row writer would silently keep the class name"
            );
        }
    }

    /// The two single-writer binaries must not look like provider classes, so
    /// nobody is tempted to scope them: there is only ever one of each, and
    /// scoping would orphan the fields the unscoped name already owns.
    #[test]
    fn the_single_writer_binaries_are_not_provider_classes() {
        for manager in [FIELD_MANAGER_CONTROLLER, FIELD_MANAGER_IMAGEBUILDER] {
            assert!(
                !manager.contains("provider-"),
                "{manager} is a single-writer binary and must not look like a provider class"
            );
        }
    }

    /// All six base managers must be distinct, or two binaries would collide
    /// before per-writer scoping even applies.
    #[test]
    fn the_base_managers_are_all_distinct() {
        let all = [
            FIELD_MANAGER_CONTROLLER,
            FIELD_MANAGER_IMAGEBUILDER,
            FIELD_MANAGER_PROVIDER_VSPHERE,
            FIELD_MANAGER_PROVIDER_PROXMOX,
            FIELD_MANAGER_PROVIDER_LIBVIRT,
            FIELD_MANAGER_PROVIDER_CLOUD_HYPERVISOR,
        ];
        let unique: std::collections::BTreeSet<&str> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len());
    }
}
