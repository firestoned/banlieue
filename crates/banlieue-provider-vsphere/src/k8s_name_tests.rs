// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn joins_and_slugifies_short_parts() {
        assert_eq!(
            collision_safe_name(&["prod-vsphere", "DC East", "Cluster A"]),
            "prod-vsphere-dc-east-cluster-a"
        );
    }

    #[test]
    fn strips_special_characters() {
        assert_eq!(
            collision_safe_name(&["p", "dc/east", "c.l.u_s_ter:1"]),
            "p-dc-east-c-l-u-s-ter-1"
        );
    }

    #[test]
    fn collapses_consecutive_separators() {
        assert_eq!(
            collision_safe_name(&["p", "DC  East", "Cluster--A"]),
            "p-dc-east-cluster-a"
        );
    }

    #[test]
    fn truncates_to_the_dns_label_limit() {
        let huge = "x".repeat(200);
        let name = collision_safe_name(&["p", &huge, &huge]);
        assert!(
            name.len() <= MAX_NAME_LEN,
            "name too long: {} chars",
            name.len()
        );
    }

    #[test]
    fn stays_unique_when_truncated_and_parts_share_a_long_prefix() {
        // Regression: enterprise cluster names can be long and share a common
        // prefix, differing only in a trailing suffix. That suffix falls past
        // the 63-char cap, so naive front-truncation collapsed every input
        // onto one identical name.
        let dc = "dc-example";
        let base = "compute-cluster-dedicated-nonreplicated-availability-domain";
        let n1 = collision_safe_name(&["vcenter-example", dc, &format!("{base}-01")]);
        let n2 = collision_safe_name(&["vcenter-example", dc, &format!("{base}-02")]);
        let n3 = collision_safe_name(&["vcenter-example", dc, &format!("{base}-03")]);
        assert!(n1.len() <= MAX_NAME_LEN && n2.len() <= MAX_NAME_LEN && n3.len() <= MAX_NAME_LEN);
        assert_ne!(n1, n2);
        assert_ne!(n2, n3);
        assert_ne!(n1, n3);
    }

    #[test]
    fn is_deterministic() {
        let huge = "y".repeat(120);
        assert_eq!(
            collision_safe_name(&["p", &huge, "cluster-01"]),
            collision_safe_name(&["p", &huge, "cluster-01"]),
        );
    }

    #[test]
    fn four_parts_stay_unique_when_truncated() {
        // import_job_name's shape: an extra leading "import" part on top of
        // the same three-field pattern above.
        let base = "compute-cluster-dedicated-nonreplicated-availability-domain";
        let n1 = collision_safe_name(&["import", "img", "vcenter-example", &format!("{base}-01")]);
        let n2 = collision_safe_name(&["import", "img", "vcenter-example", &format!("{base}-02")]);
        assert!(n1.len() <= MAX_NAME_LEN && n2.len() <= MAX_NAME_LEN);
        assert_ne!(n1, n2);
    }
}
