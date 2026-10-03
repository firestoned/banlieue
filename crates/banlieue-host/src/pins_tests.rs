// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `pins.rs`: one pinned release, everywhere.

#[cfg(test)]
mod tests {
    use super::super::*;

    const SPEC_PIN: &str = include_str!("../../banlieue-cloud-hypervisor/spec/PIN");

    fn spec_pin_version() -> String {
        SPEC_PIN
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .find_map(|l| {
                let (k, v) = l.split_once('=')?;
                (k.trim() == "version").then(|| v.trim().to_string())
            })
            .expect("spec/PIN names a version")
    }

    /// ADR-0067 Decision 4 / roadmap 09 phase 10, invariant 6: the VMM the
    /// installer puts on a host is the release the client was checked
    /// against, and the one its version gate expects.
    #[test]
    fn the_installed_vmm_is_the_release_the_client_is_pinned_to() {
        assert_eq!(VMM_VERSION, spec_pin_version());
        let gate = banlieue_cloud_hypervisor::PINNED_VERSION;
        assert_eq!(VMM_VERSION, format!("v{}.{}", gate.major, gate.minor));
    }

    #[test]
    fn every_pin_is_a_sha256() {
        for a in Release::pinned().artifacts {
            assert_eq!(a.sha256.len(), 64, "{}", a.name);
            assert!(
                a.sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{}",
                a.name
            );
            assert!(a.url.starts_with("https://"), "{}", a.url);
            assert!(a.url.ends_with(a.name), "{}", a.url);
        }
    }

    /// The symlinks point at what the stage installs, so a verified
    /// install is what the commands run.
    #[test]
    fn symlinks_point_at_installed_artifacts() {
        let r = Release::pinned();
        let dests: Vec<_> = r.artifacts.iter().map(|a| a.dest.clone()).collect();
        for (_, target) in &r.symlinks {
            assert!(dests.contains(target), "{}", target.display());
        }
    }

    /// The pinned release is laid out under its own version and tag, so
    /// another release installs beside it, never over it.
    #[test]
    fn the_pinned_release_is_laid_out_by_version_and_tag() {
        let r = Release::pinned();
        assert_eq!(r.version, VMM_VERSION);
        assert_eq!(r.firmware_tag, FIRMWARE_TAG);
        assert_eq!(
            r.artifacts[0].dest,
            std::path::Path::new("/opt/banlieue/cloud-hypervisor/v53.0/cloud-hypervisor")
        );
        assert_eq!(
            r.firmware,
            std::path::Path::new("/opt/banlieue/firmware/ch-97eeb7b09/CLOUDHV.fd")
        );
        assert_eq!(r.firmware_sha256, FIRMWARE_SHA256);
        assert_eq!(
            github_url(FIRMWARE_REPO, FIRMWARE_TAG, FIRMWARE_ASSET),
            "https://github.com/cloud-hypervisor/edk2/releases/download/ch-97eeb7b09/CLOUDHV.fd"
        );
    }

    #[test]
    fn sha256_of_nothing() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
