// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the `VMImage` reconciler's pure decisions.

#[cfg(test)]
mod tests {
    use banlieue_api::banlieue::{
        Architecture, ImageSource, ImageSourceKind, ProviderCapabilities,
    };
    use banlieue_aws::FakeEc2;
    use banlieue_aws::types::ImageInfo;

    use super::super::*;
    use crate::test_support::{self, HOME, IMAGE_ID, PROVIDER};

    fn source(kind: ImageSourceKind, reference: &str) -> ImageSource {
        ImageSource {
            provider_class: PROVIDER_CLASS_NAME.into(),
            kind,
            reference: reference.into(),
            import_from: None,
            checksum: None,
        }
    }

    fn provider() -> Provider {
        test_support::provider(ProviderCapabilities::default())
    }

    #[test]
    fn a_template_ami_is_ready_in_every_zone() {
        let row = row_for(&provider(), &source(ImageSourceKind::Template, IMAGE_ID));
        assert!(row.ready);
        assert_eq!(row.provider_name, PROVIDER);
        assert_eq!(row.provider_namespace, HOME);
        assert_eq!(row.resolved_ref.as_deref(), Some(IMAGE_ID));
        assert_eq!(row.reason.as_deref(), Some(reasons::RECONCILED));
        assert!(row.zones.is_empty(), "regional: ready in every zone");
    }

    #[test]
    fn a_template_ref_is_trimmed() {
        let padded = format!(" {IMAGE_ID}\n");
        let row = row_for(&provider(), &source(ImageSourceKind::Template, &padded));
        assert_eq!(row.resolved_ref.as_deref(), Some(IMAGE_ID));
    }

    #[test]
    fn a_short_ami_id_is_accepted() {
        let row = row_for(
            &provider(),
            &source(ImageSourceKind::Template, "ami-0123abcd"),
        );
        assert!(row.ready);
    }

    #[test]
    fn a_ref_that_is_not_an_ami_id_is_invalid() {
        for bad in ["", "ami-xyz", "snap-0123456789abcdef0", "ubuntu-24.04"] {
            let row = row_for(&provider(), &source(ImageSourceKind::Template, bad));
            assert!(!row.ready, "{bad:?}");
            assert_eq!(row.reason.as_deref(), Some(reasons::INVALID_REF), "{bad:?}");
            assert!(row.resolved_ref.is_none());
        }
    }

    #[test]
    fn url_and_other_kinds_are_unsupported_until_phase_d() {
        for kind in [
            ImageSourceKind::Url,
            ImageSourceKind::BackingFile,
            ImageSourceKind::OciArtifact,
        ] {
            let row = row_for(&provider(), &source(kind.clone(), IMAGE_ID));
            assert!(!row.ready, "{kind:?}");
            assert_eq!(
                row.reason.as_deref(),
                Some(reasons::UNSUPPORTED_SOURCE_KIND)
            );
            assert!(row.resolved_ref.is_none());
            let message = row.message.unwrap_or_default();
            assert!(message.contains("phase D"), "{message}");
        }
    }

    #[test]
    fn the_ec2_source_is_found_among_others() {
        let mut azure = source(ImageSourceKind::Template, "/subscriptions/x");
        azure.provider_class = "azure".into();
        let ec2 = source(ImageSourceKind::Template, IMAGE_ID);
        let sources = [azure, ec2.clone()];
        assert_eq!(find_ec2_source(&sources), Some(&ec2));
        assert_eq!(find_ec2_source(&sources[..1]), None);
    }

    // ---- DescribeImages (roadmap 24 phase E) ---------------------------

    fn info(id: &str) -> ImageInfo {
        ImageInfo {
            image_id: id.into(),
            state: "available".into(),
            architecture: "x86_64".into(),
            root_device_name: "/dev/sda1".into(),
            root_device_type: "ebs".into(),
            boot_mode: Some("uefi".into()),
            tpm_support: Some("v2.0".into()),
            ena_support: true,
            root_volume_size_gib: Some(10),
        }
    }

    #[test]
    fn an_available_ebs_ami_is_ready_with_its_attributes() {
        let row = row_from_image(&provider(), IMAGE_ID, Some(&info(IMAGE_ID)));
        assert!(row.ready);
        assert_eq!(row.resolved_ref.as_deref(), Some(IMAGE_ID));
        assert_eq!(row.reason.as_deref(), Some(reasons::RECONCILED));
        assert_eq!(row.min_os_disk_size_gib, Some(10), "the snapshot's size");
        let a = row.ec2.expect("attributes recorded");
        assert_eq!(a.architecture, Architecture::Amd64);
        assert_eq!(a.root_device_name, "/dev/sda1");
        assert_eq!(a.boot_mode.as_deref(), Some("uefi"));
        assert_eq!(a.tpm_support.as_deref(), Some("v2.0"));
        assert!(a.ena_support);
        assert!(a.supports_nitro_tpm());
    }

    #[test]
    fn an_arm64_ami_records_arm64() {
        let mut i = info(IMAGE_ID);
        i.architecture = "arm64".into();
        let row = row_from_image(&provider(), IMAGE_ID, Some(&i));
        assert_eq!(row.ec2.map(|a| a.architecture), Some(Architecture::Arm64));
    }

    #[test]
    fn an_ami_without_tpm_support_is_ready_and_says_so() {
        let mut i = info(IMAGE_ID);
        i.tpm_support = None;
        i.boot_mode = None;
        let row = row_from_image(&provider(), IMAGE_ID, Some(&i));
        assert!(row.ready, "a class without tpmEnabled can still boot it");
        let a = row.ec2.expect("attributes");
        assert!(!a.supports_nitro_tpm());
    }

    #[test]
    fn an_ami_ec2_does_not_know_is_refused() {
        let row = row_from_image(&provider(), IMAGE_ID, None);
        assert!(!row.ready);
        assert_eq!(row.reason.as_deref(), Some(reasons::AMI_NOT_FOUND));
        assert!(row.ec2.is_none());
        assert!(row.resolved_ref.is_none());
    }

    #[test]
    fn what_banlieue_cannot_boot_is_refused_with_a_named_reason() {
        type Mutate = fn(&mut ImageInfo);
        let cases: [(Mutate, &str, &str); 5] = [
            (
                |i| i.state = "pending".into(),
                reasons::AMI_NOT_AVAILABLE,
                "pending",
            ),
            (
                |i| i.state = "deregistered".into(),
                reasons::AMI_NOT_AVAILABLE,
                "deregistered",
            ),
            (
                |i| i.root_device_type = "instance-store".into(),
                reasons::AMI_NOT_EBS_BACKED,
                "instance-store",
            ),
            (
                |i| i.architecture = "i386".into(),
                reasons::AMI_ARCHITECTURE_UNSUPPORTED,
                "i386",
            ),
            (
                |i| i.root_device_name = "sda1".into(),
                reasons::AMI_ROOT_DEVICE_INVALID,
                "sda1",
            ),
        ];
        for (mutate, reason, named) in cases {
            let mut i = info(IMAGE_ID);
            mutate(&mut i);
            let row = row_from_image(&provider(), IMAGE_ID, Some(&i));
            assert!(!row.ready, "{reason}");
            assert_eq!(row.reason.as_deref(), Some(reason));
            assert!(row.message.unwrap_or_default().contains(named), "{reason}");
            assert!(row.ec2.is_none(), "{reason}");
        }
    }

    #[test]
    fn an_overlong_attribute_is_not_recorded() {
        let mut i = info(IMAGE_ID);
        i.boot_mode = Some("x".repeat(64));
        let row = row_from_image(&provider(), IMAGE_ID, Some(&i));
        assert_eq!(row.ec2.and_then(|a| a.boot_mode), None);
    }

    #[test]
    fn transient_errors_are_told_apart_from_refusals() {
        use banlieue_aws::{AwsFault, classify};
        let throttled = classify(AwsFault::new(503, "RequestLimitExceeded", "m", "r"));
        let server = classify(AwsFault::new(500, "InternalError", "m", "r"));
        let transport = banlieue_aws::Error::Transport("reset".into());
        let forbidden = classify(AwsFault::new(403, "UnauthorizedOperation", "m", "r"));
        let malformed = classify(AwsFault::new(400, "InvalidAMIID.Malformed", "m", "r"));
        assert!(is_transient(&throttled));
        assert!(is_transient(&server));
        assert!(is_transient(&transport));
        assert!(!is_transient(&forbidden));
        assert!(!is_transient(&malformed));
    }

    fn ready_row() -> ImagePerProviderStatus {
        row_from_image(&provider(), IMAGE_ID, Some(&info(IMAGE_ID)))
    }

    #[tokio::test]
    async fn a_template_is_described_and_recorded() {
        let fake = FakeEc2::new();
        fake.add_image(info(IMAGE_ID));
        let d = template_row(
            &fake,
            &provider(),
            &source(ImageSourceKind::Template, IMAGE_ID),
            None,
        )
        .await;
        assert!(!d.transient);
        assert_eq!(d.row, ready_row());
    }

    #[tokio::test]
    async fn an_unknown_template_ami_is_not_found() {
        let fake = FakeEc2::new();
        let d = template_row(
            &fake,
            &provider(),
            &source(ImageSourceKind::Template, IMAGE_ID),
            None,
        )
        .await;
        assert!(!d.transient);
        assert!(!d.row.ready);
        assert_eq!(d.row.reason.as_deref(), Some(reasons::AMI_NOT_FOUND));
    }

    #[tokio::test]
    async fn throttling_keeps_a_ready_row_ready() {
        let fake = FakeEc2::new();
        fake.add_image(info(IMAGE_ID));
        fake.throttle_next(1);
        let existing = ready_row();
        let d = template_row(
            &fake,
            &provider(),
            &source(ImageSourceKind::Template, IMAGE_ID),
            Some(&existing),
        )
        .await;
        assert!(d.transient);
        assert_eq!(d.row, existing, "unchanged, so nothing flips to not ready");
    }

    #[tokio::test]
    async fn throttling_with_no_row_yet_writes_an_unverified_row() {
        let fake = FakeEc2::new();
        fake.throttle_next(1);
        let d = template_row(
            &fake,
            &provider(),
            &source(ImageSourceKind::Template, IMAGE_ID),
            None,
        )
        .await;
        assert!(d.transient);
        assert!(!d.row.ready);
        assert_eq!(d.row.reason.as_deref(), Some(reasons::AMI_UNVERIFIED));
    }

    /// A row for another AMI (the `ref` changed) is not kept on a throttle.
    #[tokio::test]
    async fn throttling_does_not_keep_a_row_for_another_ami() {
        let fake = FakeEc2::new();
        fake.throttle_next(1);
        let existing = ready_row();
        let d = template_row(
            &fake,
            &provider(),
            &source(ImageSourceKind::Template, "ami-0123abcd"),
            Some(&existing),
        )
        .await;
        assert!(d.transient);
        assert!(!d.row.ready);
    }

    #[tokio::test]
    async fn a_template_that_is_not_an_ami_id_is_not_described() {
        let fake = FakeEc2::new();
        fake.throttle_next(1);
        let d = template_row(
            &fake,
            &provider(),
            &source(ImageSourceKind::Template, "ubuntu"),
            None,
        )
        .await;
        assert!(!d.transient, "no call was made");
        assert_eq!(d.row.reason.as_deref(), Some(reasons::INVALID_REF));
    }
}
