// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `host_credential.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::os::unix::fs::PermissionsExt;

    /// The host kubeconfig reads its token from a file and carries no
    /// secret of its own, so it never needs rewriting when the token turns
    /// over (ADR-0060 Decision 5).
    #[test]
    fn the_host_kubeconfig_reads_its_token_from_a_file_and_embeds_none() {
        let text = host_kubeconfig(
            "https://192.0.2.10:6443",
            Some("Q0EtREFUQQ=="),
            Path::new("/etc/banlieue/credentials/token"),
        );
        let kc = Kubeconfig::from_yaml(&text).expect("a valid kubeconfig");
        let user = kc.auth_infos[0].auth_info.as_ref().unwrap();
        assert_eq!(
            user.token_file.as_deref(),
            Some("/etc/banlieue/credentials/token")
        );
        assert!(user.token.is_none(), "no inline token");
        let cluster = kc.clusters[0].cluster.as_ref().unwrap();
        assert_eq!(cluster.server.as_deref(), Some("https://192.0.2.10:6443"));
        assert_eq!(
            cluster.certificate_authority_data.as_deref(),
            Some("Q0EtREFUQQ==")
        );
        assert_eq!(kc.current_context.as_deref(), Some("provider"));
    }

    #[test]
    fn credential_files_are_private_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        write_private(&path, "t").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(
            write_private(&path, "other").is_err(),
            "never overwrite a credential"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "t");
    }
}
