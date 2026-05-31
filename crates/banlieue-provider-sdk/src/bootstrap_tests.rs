// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::bootstrap`].
//!
//! Only the pure helpers are unit-tested here: `join_directives` (log filter
//! spec assembly) and `health_http_response` (the bytes written to a health
//! probe). The async `serve_health` / `shutdown_signal` and the global
//! `init_tracing` (which can only initialise the process subscriber once) are
//! exercised end-to-end by the running binary, not unit-tested.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn join_directives_level_only() {
        assert_eq!(join_directives("debug", &[]), "debug");
    }

    #[test]
    fn join_directives_appends_each_extra_in_order() {
        assert_eq!(
            join_directives("info", &["kube=warn", "vim_rs=warn"]),
            "info,kube=warn,vim_rs=warn",
        );
    }

    #[test]
    fn health_response_is_200_with_matching_content_length() {
        let body = "ok";
        let response = health_http_response(body);

        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Content-Type: text/plain\r\n"));
        assert!(response.contains(&format!("Content-Length: {}\r\n", body.len())));
        assert!(response.ends_with(&format!("\r\n\r\n{body}")));
    }
}
