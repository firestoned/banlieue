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
            Error::Vsphere("login to vcenter.example.com failed".into()).kind(),
            "Vsphere"
        );
        assert_eq!(Error::InvalidSpec("dup".into()).kind(), "InvalidSpec");
        assert_eq!(Error::Missing("spec.connection").kind(), "Missing");
        assert_eq!(
            Error::Sdk(banlieue_provider_sdk::Error::Missing("x")).kind(),
            "Sdk"
        );
    }
}
