// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::virtualmachine`].
//!
//! The async `reconcile` function isn't unit-testable without a fake kube
//! client; those flows are covered by the scheduler / status_mirror /
//! infra suites (each exercising the pure function it owns). The smoke
//! tests below guard the public constants and error-policy decisions.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn finalizer_constant_uses_project_domain() {
        assert!(VM_FINALIZER.starts_with("banlieue.io/"));
    }

    #[test]
    fn finalizer_constant_is_stable_string() {
        // Stable wire value — changing this WILL strand finalizers on
        // existing VMs in production and is therefore a breaking change.
        // Any modification here must come with a documented migration plan.
        assert_eq!(VM_FINALIZER, "banlieue.io/virtualmachine");
    }
}
