// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `error.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    fn api(status: u16, message: &str) -> Error {
        Error::Api {
            status,
            message: message.to_string(),
        }
    }

    #[test]
    fn api_error_display_carries_status_and_message() {
        let e = api(401, "Authentication failed!");
        assert_eq!(
            e.to_string(),
            "Proxmox returned 401: Authentication failed!"
        );
    }

    #[test]
    fn not_found_is_a_404() {
        assert!(api(404, "no such thing").is_not_found());
    }

    #[test]
    fn not_found_is_the_500_proxmox_uses_for_a_missing_config() {
        let e = api(
            500,
            "Configuration file 'nodes/pve/qemu-server/100.conf' does not exist",
        );
        assert!(e.is_not_found());
    }

    #[test]
    fn other_500s_are_not_not_found() {
        assert!(!api(500, "VM is running - destroy failed").is_not_found());
        assert!(!api(401, "Authentication failed!").is_not_found());
    }

    #[test]
    fn transport_and_task_errors_are_not_not_found() {
        assert!(!Error::Transport("refused".into()).is_not_found());
        assert!(
            !Error::TaskFailed {
                upid: "u".into(),
                exitstatus: "boom".into()
            }
            .is_not_found()
        );
    }

    #[test]
    fn unauthorized_is_401_or_403() {
        assert!(api(401, "x").is_unauthorized());
        assert!(api(403, "x").is_unauthorized());
        assert!(!api(500, "x").is_unauthorized());
    }
}
