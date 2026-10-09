// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::error`]: the metric `kind` of each variant.

#[cfg(test)]
mod tests {
    use banlieue_provider_sdk::metrics::ErrorKind;

    use super::super::*;

    #[test]
    fn kind_is_the_variant_name_not_the_message() {
        assert_eq!(
            Error::Libvirt("connect bar.foo.io:16514 refused".into()).kind(),
            "Libvirt"
        );
        assert_eq!(
            Error::Invalid {
                what: "pool",
                detail: "empty".into()
            }
            .kind(),
            "Invalid"
        );
        assert_eq!(Error::Missing("spec.uuid").kind(), "Missing");
    }
}
