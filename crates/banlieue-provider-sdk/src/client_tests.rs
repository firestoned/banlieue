// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `client.rs`: explicit kubeconfig handling, no cluster.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::io::Write;

    fn kubeconfig(dir: &std::path::Path, file: &str, server: &str, ns: &str) -> std::path::PathBuf {
        let path = dir.join(file);
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            r#"apiVersion: v1
kind: Config
clusters:
- name: c-{file}
  cluster: {{server: "{server}", insecure-skip-tls-verify: true}}
users:
- name: u-{file}
  user: {{token: "t-{file}"}}
contexts:
- name: x-{file}
  context: {{cluster: c-{file}, user: u-{file}, namespace: {ns}}}
current-context: x-{file}
"#
        )
        .unwrap();
        path
    }

    /// The bug this fixes: `--kubeconfig` was parsed and then ignored.
    #[tokio::test]
    async fn an_explicit_kubeconfig_is_the_one_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = kubeconfig(dir.path(), "a.yaml", "https://192.0.2.10:6443", "team-a");
        let config = config_from_kubeconfig(path.as_os_str()).await.unwrap();
        assert_eq!(config.cluster_url.to_string(), "https://192.0.2.10:6443/");
        assert_eq!(config.default_namespace, "team-a");
        assert_eq!(
            config.read_timeout,
            Some(Duration::from_secs(DEFAULT_READ_TIMEOUT_SECS))
        );
    }

    /// `KUBECONFIG` (which also feeds the flag) may be a list; files merge as
    /// kubectl merges them, the first file's current-context winning.
    #[tokio::test]
    async fn a_kubeconfig_list_merges_with_the_first_file_winning() {
        let dir = tempfile::tempdir().unwrap();
        let a = kubeconfig(dir.path(), "a.yaml", "https://192.0.2.10:6443", "team-a");
        let b = kubeconfig(dir.path(), "b.yaml", "https://198.51.100.20:6443", "team-b");
        let list = std::env::join_paths([&a, &b]).unwrap();
        let config = config_from_kubeconfig(&list).await.unwrap();
        assert_eq!(config.cluster_url.to_string(), "https://192.0.2.10:6443/");
    }

    #[tokio::test]
    async fn a_missing_kubeconfig_is_an_error_not_a_silent_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.yaml");
        assert!(config_from_kubeconfig(missing.as_os_str()).await.is_err());
    }
}
