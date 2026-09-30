// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `upid.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::Error;

    const CLONE: &str = "UPID:pve1:0004A2B1:03C1D2E3:6512F0A0:qmclone:100:banlieue@pve!provider:";

    #[test]
    fn a_clone_upid_parses_into_its_parts() {
        let u = Upid::parse(CLONE).unwrap();
        assert_eq!(u.node(), "pve1");
        assert_eq!(u.task_type(), "qmclone");
        assert_eq!(u.id(), "100");
        assert_eq!(u.as_str(), CLONE);
        assert_eq!(u.to_string(), CLONE);
    }

    #[test]
    fn an_empty_id_is_allowed() {
        let u = Upid::parse("UPID:pve1:1:2:3:vzdump::root@pam:").unwrap();
        assert_eq!(u.id(), "");
    }

    #[test]
    fn wrong_prefix_is_rejected() {
        assert!(matches!(
            Upid::parse("XPID:pve1:1:2:3:qmstart:100:root@pam:"),
            Err(Error::InvalidUpid(_))
        ));
    }

    #[test]
    fn wrong_field_count_is_rejected() {
        assert!(Upid::parse("UPID:pve1:1:2:3:qmstart:100:root@pam").is_err());
        assert!(Upid::parse("UPID:pve1").is_err());
        assert!(Upid::parse("").is_err());
    }

    #[test]
    fn empty_node_is_rejected() {
        assert!(Upid::parse("UPID::1:2:3:qmstart:100:root@pam:").is_err());
    }

    #[test]
    fn a_path_traversal_in_the_node_is_rejected() {
        assert!(Upid::parse("UPID:../etc:1:2:3:qmstart:100:root@pam:").is_err());
        assert!(Upid::parse("UPID:a/b:1:2:3:qmstart:100:root@pam:").is_err());
    }

    #[test]
    fn non_hex_pid_is_rejected() {
        assert!(Upid::parse("UPID:pve1:zz:2:3:qmstart:100:root@pam:").is_err());
    }
}
