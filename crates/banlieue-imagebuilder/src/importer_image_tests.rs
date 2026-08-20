// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn default_uses_the_built_in_image_and_no_pull_secrets() {
        let image = ImporterImage::default();
        assert_eq!(image.reference, ISO_OVERLAY_IMPORTER_IMAGE);
        assert!(image.pull_secrets.is_empty());
    }

    #[test]
    fn from_flags_carries_the_reference_and_pull_secrets_through() {
        let image = ImporterImage::from_flags(
            "mirror.internal/library/busybox:1.36@sha256:abc123",
            &["mirror-pull-secret".to_string()],
        );
        assert_eq!(
            image.reference,
            "mirror.internal/library/busybox:1.36@sha256:abc123"
        );
        assert_eq!(image.pull_secrets, vec!["mirror-pull-secret".to_string()]);
    }

    #[test]
    fn from_flags_with_no_pull_secrets_is_empty() {
        let image = ImporterImage::from_flags(ISO_OVERLAY_IMPORTER_IMAGE, &[]);
        assert!(image.pull_secrets.is_empty());
    }
}
