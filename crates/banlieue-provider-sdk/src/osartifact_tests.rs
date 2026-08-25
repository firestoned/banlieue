// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `osartifact.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn owner_references_none_when_uid_unknown() {
        assert!(owner_references("kairos-rhel98-build", None).is_none());
    }

    #[test]
    fn owner_references_builds_single_entry_when_uid_known() {
        let refs = owner_references(
            "kairos-rhel98-build",
            Some("11111111-2222-3333-4444-555555555555"),
        )
        .expect("uid was Some");
        let arr = refs.as_array().expect("owner references is an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["apiVersion"], OSARTIFACT_API_VERSION);
        assert_eq!(arr[0]["kind"], OSARTIFACT_KIND);
        assert_eq!(arr[0]["name"], "kairos-rhel98-build");
        assert_eq!(arr[0]["uid"], "11111111-2222-3333-4444-555555555555");
        // blockOwnerDeletion deliberately omitted — see the doc comment.
        assert!(arr[0].get("blockOwnerDeletion").is_none());
    }
}
