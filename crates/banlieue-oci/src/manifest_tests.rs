// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `manifest.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    fn layer(media_type: &str) -> Descriptor {
        Descriptor {
            media_type: media_type.into(),
            digest: sha256_digest(b"x"),
            size: 1,
            annotations: BTreeMap::new(),
        }
    }

    /// The empty descriptor is the OCI spec's well-known value.
    #[test]
    fn the_config_is_the_oci_empty_descriptor() {
        let m = Manifest::artifact(
            ARTIFACT_TYPE_RAW,
            layer(LAYER_MEDIA_TYPE_GZIP),
            BTreeMap::new(),
        );
        assert_eq!(m.config.media_type, EMPTY_MEDIA_TYPE);
        assert_eq!(
            m.config.digest,
            "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
        assert_eq!(m.config.size, 2);
    }

    #[test]
    fn the_manifest_serializes_in_oci_shape() {
        let m = Manifest::artifact(
            ARTIFACT_TYPE_RAW,
            layer(LAYER_MEDIA_TYPE_GZIP),
            BTreeMap::from([("io.banlieue.vmimage".into(), "kairos".into())]),
        );
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["schemaVersion"], 2);
        assert_eq!(v["mediaType"], MANIFEST_MEDIA_TYPE);
        assert_eq!(v["artifactType"], ARTIFACT_TYPE_RAW);
        assert_eq!(v["layers"][0]["mediaType"], LAYER_MEDIA_TYPE_GZIP);
        assert_eq!(v["annotations"]["io.banlieue.vmimage"], "kairos");
        let back: Manifest = serde_json::from_value(v).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn only_a_single_gzip_layer_is_accepted() {
        let ok = Manifest::artifact(
            ARTIFACT_TYPE_RAW,
            layer(LAYER_MEDIA_TYPE_GZIP),
            BTreeMap::new(),
        );
        assert!(ok.single_layer().is_ok());
        let wrong_type = Manifest::artifact(
            ARTIFACT_TYPE_RAW,
            layer("application/octet-stream"),
            BTreeMap::new(),
        );
        assert!(wrong_type.single_layer().is_err());
        let mut two = ok.clone();
        two.layers.push(layer(LAYER_MEDIA_TYPE_GZIP));
        assert!(two.single_layer().is_err());
    }

    #[test]
    fn hex_is_lowercase_two_digits_per_byte() {
        assert_eq!(hex(&[0, 15, 255]), "000fff");
    }

    /// The uncompressed size is part of the layer descriptor, which the
    /// manifest digest covers; a layer without it is refused.
    #[test]
    fn the_uncompressed_size_is_read_from_the_layer() {
        let mut layer = Descriptor {
            media_type: LAYER_MEDIA_TYPE_GZIP.to_string(),
            digest: sha256_digest(b"x"),
            size: 1,
            annotations: BTreeMap::new(),
        };
        assert!(uncompressed_size(&layer).is_err());
        layer.annotations.insert(
            ANNOTATION_UNCOMPRESSED_SIZE.to_string(),
            "21474836480".into(),
        );
        assert_eq!(uncompressed_size(&layer).unwrap(), 21_474_836_480);
        layer
            .annotations
            .insert(ANNOTATION_UNCOMPRESSED_SIZE.to_string(), "-1".into());
        assert!(uncompressed_size(&layer).is_err());
    }
}
