// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `naming.rs`. The operator's `naming_tests.rs` covers the
//! same functions through its re-export.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn workload_name_is_prefixed_class_then_provider() {
        assert_eq!(
            workload_name("cloud-hypervisor", "grill-a"),
            "banlieue-provider-cloud-hypervisor-grill-a"
        );
    }

    #[test]
    fn long_names_are_truncated_to_a_valid_length_with_a_stable_hash() {
        let long = "x".repeat(80);
        let a = workload_name("cloud-hypervisor", &long);
        assert_eq!(a.len(), MAX_NAME_LEN);
        assert_eq!(a, workload_name("cloud-hypervisor", &long), "deterministic");
        assert_ne!(a, workload_name("cloud-hypervisor", &format!("{long}y")));
    }

    #[test]
    fn cluster_scoped_names_include_the_namespace() {
        assert_ne!(
            cluster_scoped_name("c", "ns-a", "p"),
            cluster_scoped_name("c", "ns-b", "p")
        );
    }
}
