// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `auth.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn a_bearer_challenge_parses_with_commas_inside_quotes() {
        let c = parse_challenge(
            r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="repository:org/disk:pull,push""#,
        )
        .unwrap();
        assert_eq!(
            c,
            Challenge::Bearer {
                realm: "https://ghcr.io/token".into(),
                service: Some("ghcr.io".into()),
                scope: Some("repository:org/disk:pull,push".into()),
            }
        );
    }

    #[test]
    fn basic_and_unknown_schemes() {
        assert_eq!(
            parse_challenge(r#"Basic realm="registry""#).unwrap(),
            Challenge::Basic
        );
        assert!(parse_challenge("Negotiate abc").is_err());
        assert!(
            parse_challenge(r#"Bearer service="x""#).is_err(),
            "no realm"
        );
    }

    #[test]
    fn credentials_never_print_the_password() {
        let c = Credentials {
            username: Some("robot".into()),
            password: Some("s3cr3t".into()),
        };
        let shown = format!("{c:?}");
        assert!(
            shown.contains("robot") && !shown.contains("s3cr3t"),
            "{shown}"
        );
        assert!(Credentials::anonymous().is_anonymous());
    }

    /// A mounted `kubernetes.io/basic-auth` Secret: trailing newlines from
    /// hand-made files are dropped, an absent directory is anonymous.
    #[test]
    fn credentials_load_from_a_basic_auth_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(USERNAME_FILE), "robot\n").unwrap();
        std::fs::write(dir.path().join(PASSWORD_FILE), "s3cret").unwrap();
        let c = Credentials::from_dir(dir.path()).unwrap();
        assert_eq!(c.username.as_deref(), Some("robot"));
        assert_eq!(c.password.as_deref(), Some("s3cret"));
        assert!(!format!("{c:?}").contains("s3cret"));

        let empty = tempfile::tempdir().unwrap();
        assert!(Credentials::from_dir(empty.path()).unwrap().is_anonymous());
    }

    /// A password with no username (or the reverse) is a broken Secret,
    /// not an anonymous pull.
    #[test]
    fn half_a_credential_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PASSWORD_FILE), "s3cret").unwrap();
        assert!(Credentials::from_dir(dir.path()).is_err());
    }
}
