// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `release.rs` (ADR-0084 Decisions 2 to 4).

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::pins::{
        self, CH_REMOTE_SHA256, FIRMWARE_SHA256, FIRMWARE_TAG, VMM_SHA256, VMM_VERSION,
    };
    use async_trait::async_trait;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Mutex;

    const NEWER: &str = "v54.1";
    const NEWER_FIRMWARE: &str = "ch-0123456789";
    const VMM_DIGEST: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const REMOTE_DIGEST: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const FIRMWARE_DIGEST: &str =
        "3333333333333333333333333333333333333333333333333333333333333333";
    const MIRROR: &str = "https://internal.example.com/artifactory/vcs-github";

    /// GitHub's published digests, canned; records every lookup.
    #[derive(Default)]
    struct Published {
        releases: BTreeMap<(String, String, String), BTreeMap<String, String>>,
        asked: Mutex<Vec<String>>,
    }

    impl Published {
        fn upstream() -> Self {
            let mut releases = BTreeMap::new();
            releases.insert(
                (
                    "cloud-hypervisor".into(),
                    "cloud-hypervisor".into(),
                    NEWER.into(),
                ),
                BTreeMap::from([
                    ("cloud-hypervisor-static".into(), VMM_DIGEST.into()),
                    ("ch-remote-static".into(), REMOTE_DIGEST.into()),
                ]),
            );
            releases.insert(
                (
                    "cloud-hypervisor".into(),
                    "edk2".into(),
                    NEWER_FIRMWARE.into(),
                ),
                BTreeMap::from([("CLOUDHV.fd".into(), FIRMWARE_DIGEST.into())]),
            );
            Self {
                releases,
                ..Self::default()
            }
        }

        fn asked(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Digests for Published {
        async fn published(
            &self,
            org: &str,
            repo: &str,
            tag: &str,
        ) -> Result<BTreeMap<String, String>, String> {
            self.asked
                .lock()
                .unwrap()
                .push(format!("{org}/{repo}@{tag}"));
            self.releases
                .get(&(org.into(), repo.into(), tag.into()))
                .cloned()
                .ok_or_else(|| "api.github.com: unreachable".to_string())
        }
    }

    fn artifact<'a>(r: &'a Release, name: &str) -> &'a pins::Artifact {
        r.artifacts.iter().find(|a| a.name == name).unwrap()
    }

    fn setting_name(e: &Error) -> &'static str {
        match e {
            Error::Setting { name, .. } => name,
            other => panic!("not a setting error: {other}"),
        }
    }

    // ------------------------------------------------------------- pinned

    /// No flags is the pinned release, with no network lookup.
    #[tokio::test]
    async fn no_flags_is_the_pinned_release() {
        let gh = Published::upstream();
        let r = resolve(&ReleaseArgs::default(), &gh).await.unwrap();
        assert_eq!(r, Release::pinned());
        assert!(gh.asked().is_empty());
    }

    /// A mirror changes where the bytes come from, never what they must
    /// hash to (ADR-0084 Decision 4.1).
    #[tokio::test]
    async fn a_mirror_url_keeps_the_compiled_pins() {
        let vmm_url = format!(
            "{MIRROR}/cloud-hypervisor/cloud-hypervisor/{VMM_VERSION}/cloud-hypervisor-static"
        );
        let fw_url = format!("{MIRROR}/cloud-hypervisor/edk2/{FIRMWARE_TAG}/CLOUDHV.fd");
        let args = ReleaseArgs {
            vmm_url: Some(vmm_url.clone()),
            firmware_url: Some(fw_url.clone()),
            ..ReleaseArgs::default()
        };
        let gh = Published::upstream();
        let r = resolve(&args, &gh).await.unwrap();
        let vmm = artifact(&r, "cloud-hypervisor-static");
        assert_eq!(vmm.url, vmm_url);
        assert_eq!(vmm.sha256, VMM_SHA256);
        let fw = artifact(&r, "CLOUDHV.fd");
        assert_eq!(fw.url, fw_url);
        assert_eq!(fw.sha256, FIRMWARE_SHA256);
        // Not overridden: still github.com.
        let remote = artifact(&r, "ch-remote-static");
        assert!(remote.url.starts_with(
            "https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/"
        ));
        assert_eq!(remote.sha256, CH_REMOTE_SHA256);
        assert!(gh.asked().is_empty());
    }

    /// A digest flag cannot quietly replace a compiled pin.
    #[tokio::test]
    async fn a_digest_flag_for_a_pinned_file_is_refused() {
        for (args, flag) in [
            (
                ReleaseArgs {
                    vmm_sha256: Some(VMM_DIGEST.into()),
                    ..ReleaseArgs::default()
                },
                "--vmm-sha256",
            ),
            (
                ReleaseArgs {
                    ch_remote_sha256: Some(REMOTE_DIGEST.into()),
                    ..ReleaseArgs::default()
                },
                "--ch-remote-sha256",
            ),
            (
                ReleaseArgs {
                    firmware_sha256: Some(FIRMWARE_DIGEST.into()),
                    ..ReleaseArgs::default()
                },
                "--firmware-sha256",
            ),
        ] {
            let err = resolve(&args, &Published::upstream()).await.unwrap_err();
            assert_eq!(setting_name(&err), flag, "{err}");
        }
    }

    /// Naming the pinned version explicitly is the same as not naming it.
    #[tokio::test]
    async fn naming_the_pinned_version_is_the_pinned_release() {
        let args = ReleaseArgs {
            vmm_version: Some(VMM_VERSION.into()),
            firmware_tag: Some(FIRMWARE_TAG.into()),
            ..ReleaseArgs::default()
        };
        assert_eq!(
            resolve(&args, &Published::upstream()).await.unwrap(),
            Release::pinned()
        );
    }

    // ------------------------------------------------------------ versions

    /// A VMM the provider would refuse is refused before anything is
    /// fetched or looked up (ADR-0084 Decision 3).
    #[tokio::test]
    async fn a_vmm_older_than_the_client_gate_is_refused() {
        let gh = Published::upstream();
        for old in ["v52.0", "v53", "52.9.9"] {
            let args = ReleaseArgs {
                vmm_version: Some(old.into()),
                vmm_sha256: Some(VMM_DIGEST.into()),
                ch_remote_sha256: Some(REMOTE_DIGEST.into()),
                ..ReleaseArgs::default()
            };
            let err = resolve(&args, &gh).await.unwrap_err();
            assert_eq!(setting_name(&err), "--vmm-version", "{old}: {err}");
        }
        assert!(gh.asked().is_empty());
    }

    /// The version and tag become directory names under /opt: nothing
    /// that could leave it, and nothing that is not a version.
    #[tokio::test]
    async fn unsafe_or_unparseable_versions_and_tags_are_refused() {
        for (args, flag) in [
            (
                ReleaseArgs {
                    vmm_version: Some("../../etc".into()),
                    ..ReleaseArgs::default()
                },
                "--vmm-version",
            ),
            (
                ReleaseArgs {
                    vmm_version: Some("latest".into()),
                    ..ReleaseArgs::default()
                },
                "--vmm-version",
            ),
            (
                ReleaseArgs {
                    firmware_tag: Some("../../etc".into()),
                    ..ReleaseArgs::default()
                },
                "--firmware-tag",
            ),
            (
                ReleaseArgs {
                    firmware_tag: Some(".hidden".into()),
                    ..ReleaseArgs::default()
                },
                "--firmware-tag",
            ),
            (
                ReleaseArgs {
                    firmware_tag: Some(String::new()),
                    ..ReleaseArgs::default()
                },
                "--firmware-tag",
            ),
        ] {
            let err = resolve(&args, &Published::upstream()).await.unwrap_err();
            assert_eq!(setting_name(&err), flag, "{err}");
        }
    }

    /// A newer version installs side by side: its own directory, the same
    /// symlinks, GitHub's digests when no flag gives one.
    #[tokio::test]
    async fn a_newer_version_takes_githubs_published_digests() {
        let args = ReleaseArgs {
            vmm_version: Some(NEWER.into()),
            ..ReleaseArgs::default()
        };
        let gh = Published::upstream();
        let r = resolve(&args, &gh).await.unwrap();
        assert_eq!(r.version, NEWER);
        let vmm = artifact(&r, "cloud-hypervisor-static");
        assert_eq!(vmm.sha256, VMM_DIGEST);
        assert_eq!(
            vmm.url,
            format!(
                "https://github.com/cloud-hypervisor/cloud-hypervisor/releases/download/{NEWER}/cloud-hypervisor-static"
            )
        );
        assert_eq!(
            vmm.dest,
            Path::new("/opt/banlieue/cloud-hypervisor")
                .join(NEWER)
                .join("cloud-hypervisor")
        );
        assert_eq!(artifact(&r, "ch-remote-static").sha256, REMOTE_DIGEST);
        // The firmware stayed pinned.
        assert_eq!(artifact(&r, "CLOUDHV.fd").sha256, FIRMWARE_SHA256);
        for (_, target) in &r.symlinks {
            assert!(target.starts_with(Path::new("/opt/banlieue/cloud-hypervisor").join(NEWER)));
        }
        // One lookup for both VMM files, none for the pinned firmware.
        assert_eq!(
            gh.asked(),
            vec![format!("cloud-hypervisor/cloud-hypervisor@{NEWER}")]
        );
    }

    #[tokio::test]
    async fn a_newer_firmware_tag_takes_the_edk2_digest() {
        let args = ReleaseArgs {
            firmware_tag: Some(NEWER_FIRMWARE.into()),
            ..ReleaseArgs::default()
        };
        let gh = Published::upstream();
        let r = resolve(&args, &gh).await.unwrap();
        assert_eq!(r.firmware_tag, NEWER_FIRMWARE);
        assert_eq!(r.firmware_sha256, FIRMWARE_DIGEST);
        assert_eq!(
            r.firmware,
            Path::new("/opt/banlieue/firmware")
                .join(NEWER_FIRMWARE)
                .join("CLOUDHV.fd")
        );
        assert_eq!(artifact(&r, "CLOUDHV.fd").dest, r.firmware);
        assert_eq!(
            gh.asked(),
            vec![format!("cloud-hypervisor/edk2@{NEWER_FIRMWARE}")]
        );
    }

    /// Air-gapped: every digest from a flag, nothing looked up. Uppercase
    /// hex is accepted and normalized.
    #[tokio::test]
    async fn digest_flags_need_no_lookup() {
        let args = ReleaseArgs {
            vmm_version: Some(NEWER.into()),
            vmm_url: Some(format!("{MIRROR}/vmm")),
            ch_remote_url: Some(format!("{MIRROR}/ch-remote")),
            vmm_sha256: Some(VMM_DIGEST.to_uppercase()),
            ch_remote_sha256: Some(REMOTE_DIGEST.into()),
            ..ReleaseArgs::default()
        };
        let gh = Published::default();
        let r = resolve(&args, &gh).await.unwrap();
        assert_eq!(artifact(&r, "cloud-hypervisor-static").sha256, VMM_DIGEST);
        assert_eq!(
            artifact(&r, "ch-remote-static").url,
            format!("{MIRROR}/ch-remote")
        );
        assert!(gh.asked().is_empty());
    }

    /// When GitHub cannot be asked, the error names the flag to pass.
    #[tokio::test]
    async fn an_unreachable_github_names_the_digest_flag() {
        let args = ReleaseArgs {
            vmm_version: Some(NEWER.into()),
            vmm_sha256: Some(VMM_DIGEST.into()),
            ..ReleaseArgs::default()
        };
        let err = resolve(&args, &Published::default()).await.unwrap_err();
        assert_eq!(setting_name(&err), "--ch-remote-sha256", "{err}");
        assert!(err.to_string().contains("unreachable"), "{err}");
    }

    /// A release GitHub knows but without the asset is not a digest.
    #[tokio::test]
    async fn a_release_without_the_asset_names_the_digest_flag() {
        let mut gh = Published::upstream();
        gh.releases.values_mut().for_each(|assets| {
            assets.remove("ch-remote-static");
        });
        let args = ReleaseArgs {
            vmm_version: Some(NEWER.into()),
            ..ReleaseArgs::default()
        };
        let err = resolve(&args, &gh).await.unwrap_err();
        assert_eq!(setting_name(&err), "--ch-remote-sha256", "{err}");
    }

    #[tokio::test]
    async fn a_malformed_digest_is_refused() {
        for bad in ["abc", &"g".repeat(64), &format!("sha256:{VMM_DIGEST}")] {
            let args = ReleaseArgs {
                vmm_version: Some(NEWER.into()),
                vmm_sha256: Some(bad.to_string()),
                ch_remote_sha256: Some(REMOTE_DIGEST.into()),
                ..ReleaseArgs::default()
            };
            let err = resolve(&args, &Published::default()).await.unwrap_err();
            assert_eq!(setting_name(&err), "--vmm-sha256", "{bad}: {err}");
        }
    }

    // ---------------------------------------------------------------- urls

    /// Downloads are HTTPS only (ADR-0067 Decision 4), refused up front
    /// rather than at fetch time.
    #[tokio::test]
    async fn a_url_that_is_not_https_is_refused() {
        for (args, flag) in [
            (
                ReleaseArgs {
                    vmm_url: Some("http://internal.example.com/vmm".into()),
                    ..ReleaseArgs::default()
                },
                "--vmm-url",
            ),
            (
                ReleaseArgs {
                    ch_remote_url: Some("ftp://internal.example.com/ch-remote".into()),
                    ..ReleaseArgs::default()
                },
                "--ch-remote-url",
            ),
            (
                ReleaseArgs {
                    firmware_url: Some("https://internal.example.com/a b".into()),
                    ..ReleaseArgs::default()
                },
                "--firmware-url",
            ),
        ] {
            let err = resolve(&args, &Published::upstream()).await.unwrap_err();
            assert_eq!(setting_name(&err), flag, "{err}");
        }
    }

    // --------------------------------------------------------------- github

    #[test]
    fn githubs_release_json_yields_each_assets_digest() {
        let json = br#"{"tag_name":"v53.0","assets":[
            {"name":"cloud-hypervisor-static","digest":"sha256:448af3d4e59b22c2987f7df94c213ad40fb53a10d437e42b5ee6c4fce7c29ecc"},
            {"name":"no-digest-yet","digest":null},
            {"name":"other-algorithm","digest":"sha512:abcd"}
        ]}"#;
        let digests = parse_published_digests(json).unwrap();
        assert_eq!(
            digests,
            BTreeMap::from([(
                "cloud-hypervisor-static".to_string(),
                VMM_SHA256.to_string()
            )])
        );
        assert!(parse_published_digests(b"not json").is_err());
        assert!(parse_published_digests(br#"{"message":"Not Found"}"#).is_err());
    }
}
