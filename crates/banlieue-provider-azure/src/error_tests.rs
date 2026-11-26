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
            Error::Invalid {
                what: "connection",
                detail: "spec.connection.azure is not set".into()
            }
            .kind(),
            "Invalid"
        );
        assert_eq!(Error::Missing("metadata.uid").kind(), "Missing");
        assert_eq!(
            Error::Azure(banlieue_azure::Error::Transport("refused".into())).kind(),
            "Azure"
        );
        assert_eq!(
            Error::Sdk(banlieue_provider_sdk::Error::Missing("x")).kind(),
            "Sdk"
        );
    }
}
