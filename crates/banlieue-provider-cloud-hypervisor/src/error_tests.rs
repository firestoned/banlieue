// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::error`]: the metric `kind` of each variant.

#[cfg(test)]
mod tests {
    use banlieue_provider_sdk::metrics::ErrorKind;

    use super::super::*;

    #[test]
    fn kind_is_the_variant_name_not_the_message() {
        assert_eq!(Error::UidRangeFull.kind(), "UidRangeFull");
        assert_eq!(
            Error::VmmExited("result=exit-code".into()).kind(),
            "VmmExited"
        );
        assert_eq!(Error::Systemd("refused".into()).kind(), "Systemd");
        assert_eq!(Error::Import("no pool".into()).kind(), "Import");
        assert_eq!(Error::Missing("spec.classRef").kind(), "Missing");
        assert_eq!(Error::Io(std::io::Error::other("disk full")).kind(), "Io");
    }
}
