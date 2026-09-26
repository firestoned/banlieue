// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `reference.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    const D: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn registry_repository_tag_and_digest_parse() {
        let r = Reference::parse("ghcr.io/firestoned/banlieue-images:kairos-7").unwrap();
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "firestoned/banlieue-images");
        assert_eq!(r.tag.as_deref(), Some("kairos-7"));
        assert!(r.digest.is_none());

        let r = Reference::parse(&format!("192.0.2.5:5000/disks/kairos@{D}")).unwrap();
        assert_eq!(r.registry, "192.0.2.5:5000");
        assert_eq!(r.repository, "disks/kairos");
        assert_eq!(r.digest.as_deref(), Some(D));
        assert_eq!(r.to_string(), format!("192.0.2.5:5000/disks/kairos@{D}"));
    }

    /// No implicit Docker Hub: a bare name is refused, not guessed.
    #[test]
    fn a_reference_without_a_registry_is_refused() {
        assert!(Reference::parse("library/ubuntu:24.04").is_err());
        assert!(Reference::parse("ubuntu").is_err());
        assert!(Reference::parse("localhost/x:y").is_ok());
    }

    #[test]
    fn bad_digests_tags_and_repositories_are_refused() {
        assert!(Reference::parse("ghcr.io/a/b@sha256:short").is_err());
        assert!(Reference::parse("ghcr.io/a/b@md5:0123").is_err());
        assert!(Reference::parse(&format!("ghcr.io/a/b@{}", D.to_uppercase())).is_err());
        assert!(
            Reference::parse("ghcr.io/A/b:t").is_err(),
            "uppercase repository"
        );
        assert!(
            Reference::parse("ghcr.io/a//b:t").is_err(),
            "empty component"
        );
        assert!(Reference::parse("ghcr.io/a/b:t t").is_err(), "space in tag");
    }

    #[test]
    fn with_digest_drops_the_tag() {
        let r = Reference::parse("ghcr.io/a/b:t").unwrap().with_digest(D);
        assert_eq!(r.to_string(), format!("ghcr.io/a/b@{D}"));
    }
}
