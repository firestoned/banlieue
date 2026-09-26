// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `wire.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::Error;

    // ------------------------------------------------------------------
    // Endpoints
    // ------------------------------------------------------------------

    #[test]
    fn ping_and_info_are_gets_and_everything_else_is_a_put() {
        assert_eq!(Endpoint::VmmPing.method(), http::Method::GET);
        assert_eq!(Endpoint::VmInfo.method(), http::Method::GET);
        for e in [
            Endpoint::VmCreate,
            Endpoint::VmBoot,
            Endpoint::VmPowerButton,
            Endpoint::VmShutdown,
            Endpoint::VmDelete,
            Endpoint::VmmShutdown,
            Endpoint::VmRemoveDevice,
        ] {
            assert_eq!(e.method(), http::Method::PUT, "{e:?}");
        }
    }

    #[test]
    fn paths_are_the_upstream_names_under_api_v1() {
        assert_eq!(Endpoint::VmmPing.path(), "/api/v1/vmm.ping");
        assert_eq!(Endpoint::VmCreate.path(), "/api/v1/vm.create");
        assert_eq!(Endpoint::VmPowerButton.path(), "/api/v1/vm.power-button");
        assert_eq!(Endpoint::VmRemoveDevice.path(), "/api/v1/vm.remove-device");
        assert_eq!(Endpoint::VmmShutdown.path(), "/api/v1/vmm.shutdown");
    }

    #[test]
    fn remove_device_names_the_device() {
        let body: serde_json::Value =
            serde_json::from_slice(&encode_remove_device("install")).unwrap();
        assert_eq!(body, serde_json::json!({ "id": "install" }));
    }

    // ------------------------------------------------------------------
    // Ping and the version gate (ADR-0061 Decision 5)
    // ------------------------------------------------------------------

    /// A real v53.0 VMM reports `version: "53.0.0"`; the `v`-prefixed form
    /// is only in `build_version`.
    #[test]
    fn a_real_ping_decodes() {
        let ping = decode_ping(include_bytes!("../tests/fixtures/vmm-ping-v53.0.json")).unwrap();
        assert_eq!(ping.version, "53.0.0");
        assert_eq!(ping.build_version.as_deref(), Some("v53.0"));
    }

    #[test]
    fn versions_parse_with_or_without_a_v_and_a_suffix() {
        let want = VmmVersion {
            major: 53,
            minor: 0,
            patch: 0,
        };
        assert_eq!(parse_version("53.0.0"), Some(want));
        assert_eq!(parse_version("v53.0.0"), Some(want));
        assert_eq!(parse_version("53.0"), Some(want));
        assert_eq!(parse_version("53.0.0-dirty"), Some(want));
        assert_eq!(parse_version("nonsense"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn the_pinned_version_passes_the_gate() {
        let ping = decode_ping(include_bytes!("../tests/fixtures/vmm-ping-v53.0.json")).unwrap();
        assert_eq!(check_version(&ping).unwrap(), PINNED_VERSION);
    }

    #[test]
    fn a_newer_vmm_passes_the_gate() {
        let ping = VmmPing {
            version: "54.1.0".into(),
            build_version: None,
        };
        assert!(check_version(&ping).is_ok());
    }

    /// Refuse to drive a VMM older than the one the types were written for.
    #[test]
    fn an_older_vmm_is_refused() {
        let ping = VmmPing {
            version: "52.9.0".into(),
            build_version: None,
        };
        assert!(matches!(
            check_version(&ping),
            Err(Error::VersionUnsupported { .. })
        ));
    }

    #[test]
    fn an_unparseable_version_is_refused() {
        let ping = VmmPing {
            version: "who-knows".into(),
            build_version: None,
        };
        assert!(matches!(
            check_version(&ping),
            Err(Error::VersionUnsupported { .. })
        ));
    }

    // ------------------------------------------------------------------
    // Errors from the VMM
    // ------------------------------------------------------------------

    /// Captured from a real VMM: a second vm.create is HTTP 500 with a JSON
    /// array of messages.
    #[test]
    fn a_real_api_error_keeps_every_message() {
        let e = api_error(
            500,
            include_bytes!("../tests/fixtures/error-vm-already-created-v53.0.json"),
        );
        let Error::Api { status, messages } = e else {
            panic!("expected Error::Api, got {e:?}");
        };
        assert_eq!(status, 500);
        assert_eq!(
            messages,
            vec![
                "Error from API",
                "The VM could not be created",
                "VM is already created"
            ]
        );
    }

    /// Not every failure has the array shape (a proxy, a crash mid-reply).
    /// The raw body is kept rather than lost.
    #[test]
    fn a_non_array_error_body_is_kept_verbatim() {
        let Error::Api { messages, .. } = api_error(502, b"bad gateway") else {
            panic!("expected Error::Api");
        };
        assert_eq!(messages, vec!["bad gateway"]);
    }

    /// vm.info on a VM that does not exist is a 404, which the provider needs
    /// to tell apart from a real failure.
    #[test]
    fn not_created_is_recognisable() {
        let e = api_error(
            404,
            include_bytes!("../tests/fixtures/error-vm-not-created-v53.0.json"),
        );
        assert!(e.is_not_found(), "{e:?}");
        assert!(!api_error(500, b"[]").is_not_found());
    }
}
