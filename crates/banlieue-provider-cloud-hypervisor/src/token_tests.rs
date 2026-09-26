// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `token.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    /// A token shaped like a bound ServiceAccount token. The signature is
    /// junk: the provider only reads its own token's claims, never verifies
    /// them — the API server does that.
    fn token(sub: &str, iat: i64, exp: i64) -> String {
        let b64 = |v: serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
        };
        format!(
            "{}.{}.c2ln",
            b64(serde_json::json!({"alg": "RS256"})),
            b64(serde_json::json!({"sub": sub, "iat": iat, "exp": exp}))
        )
    }

    #[test]
    fn claims_name_the_service_account_and_the_lifetime() {
        let t = token(
            "system:serviceaccount:banlieue-system:banlieue-provider-cloud-hypervisor-a",
            1_000,
            1_000 + 86_400,
        );
        let c = Claims::parse(&t).unwrap();
        assert_eq!(c.iat, 1_000);
        assert_eq!(c.exp, 87_400);
        assert_eq!(
            c.service_account(),
            Some(("banlieue-system", "banlieue-provider-cloud-hypervisor-a"))
        );
    }

    #[test]
    fn a_non_service_account_or_malformed_token_is_refused() {
        assert!(Claims::parse("not-a-jwt").is_err());
        assert!(Claims::parse("a.!!!.c").is_err());
        let user = Claims::parse(&token("alice", 0, 10)).unwrap();
        assert_eq!(user.service_account(), None);
    }

    /// Renew at half-life (ADR-0060 Decision 5): far enough ahead that a
    /// few failed attempts still leave a valid token, never so often that
    /// every restart mints a new one.
    #[test]
    fn renewal_is_at_half_life_and_immediate_once_past_it() {
        let c = Claims {
            sub: String::new(),
            iat: 1_000,
            exp: 1_000 + 86_400,
        };
        assert_eq!(c.renew_in(1_000), std::time::Duration::from_secs(43_200));
        assert_eq!(c.renew_in(40_000), std::time::Duration::from_secs(4_200));
        assert_eq!(c.renew_in(50_000), std::time::Duration::ZERO);
        assert_eq!(c.renew_in(999_999), std::time::Duration::ZERO);
    }

    /// The token file comes from the kubeconfig the host config names. A
    /// kubeconfig with an inline token cannot be renewed without rewriting
    /// it, so renewal is off for it rather than guessing.
    #[test]
    fn the_token_file_is_read_from_the_kubeconfig() {
        let dir = tempfile::tempdir().unwrap();
        let with_file = dir.path().join("with-file");
        std::fs::write(
            &with_file,
            r"apiVersion: v1
kind: Config
clusters: [{name: c, cluster: {server: 'https://192.0.2.1:6443'}}]
users: [{name: u, user: {tokenFile: /etc/banlieue/credentials/token}}]
contexts: [{name: x, context: {cluster: c, user: u}}]
current-context: x
",
        )
        .unwrap();
        assert_eq!(
            token_file_of(&with_file).unwrap(),
            Some(std::path::PathBuf::from("/etc/banlieue/credentials/token"))
        );
        let inline = dir.path().join("inline");
        std::fs::write(
            &inline,
            r"apiVersion: v1
kind: Config
clusters: [{name: c, cluster: {server: 'https://192.0.2.1:6443'}}]
users: [{name: u, user: {token: abc}}]
contexts: [{name: x, context: {cluster: c, user: u}}]
current-context: x
",
        )
        .unwrap();
        assert_eq!(token_file_of(&inline).unwrap(), None);
    }

    /// Replaced atomically, private to the provider: a reader (the kube
    /// client reloads it every minute) sees the old token or the new one,
    /// never half of either.
    #[test]
    fn the_token_is_replaced_atomically_and_privately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"old").unwrap();
        write_token(&path, "new-token").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new-token");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n != "token")
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }
}
