// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::bootstrap`].
//!
//! The pure helpers are unit-tested here: `join_directives` (log filter spec
//! assembly) and the OTLP decisions (`otlp_enabled`, `service_name_override`,
//! `build_tracer_provider`), all over an injected environment lookup so no
//! test touches the process environment. The global `init_tracing` (which can
//! only initialise the process subscriber once) and `shutdown_signal` are
//! exercised end-to-end by the running binary.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::*;

    /// An environment holding exactly `vars`.
    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

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
    fn otlp_is_off_with_no_endpoint() {
        assert!(!otlp_enabled(env(&[])));
        assert!(!otlp_enabled(env(&[("OTEL_SERVICE_NAME", "x")])));
    }

    #[test]
    fn otlp_is_off_when_endpoint_is_blank() {
        assert!(!otlp_enabled(env(&[(OTEL_ENDPOINT_ENV, "  ")])));
    }

    #[test]
    fn otlp_is_on_with_either_endpoint() {
        assert!(otlp_enabled(env(&[(
            OTEL_ENDPOINT_ENV,
            "http://otel.bar.foo.io:4318"
        )])));
        assert!(otlp_enabled(env(&[(
            OTEL_TRACES_ENDPOINT_ENV,
            "http://192.0.2.10:4318/v1/traces"
        )])));
    }

    #[test]
    fn service_name_is_the_role_by_default() {
        assert_eq!(
            service_name_override(env(&[]), "banlieue-controller"),
            Some("banlieue-controller".to_string())
        );
    }

    #[test]
    fn service_name_is_left_to_the_sdk_when_overridden() {
        assert_eq!(
            service_name_override(
                env(&[(OTEL_SERVICE_NAME_ENV, "custom")]),
                "banlieue-operator"
            ),
            None
        );
    }

    #[test]
    fn no_provider_and_no_exporter_when_disabled() {
        let provider = build_tracer_provider("banlieue-controller", env(&[])).expect("build");
        assert!(provider.is_none());
        let guard = TracingGuard { provider };
        assert!(!guard.exporting());
        guard.shutdown();
    }

    #[tokio::test]
    async fn observability_serves_health_and_metrics_and_starts_not_ready() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Reserve two free ports, release them, then hand them over.
        let (health_port, metrics_port) = {
            let a = crate::httpd::bind(0).await.expect("bind");
            let b = crate::httpd::bind(0).await.expect("bind");
            (
                a.local_addr().expect("addr").port(),
                b.local_addr().expect("addr").port(),
            )
        };
        let obs = start_observability(
            "banlieue-test",
            health_port,
            metrics_port,
            crate::health::Election::Disabled,
        )
        .await
        .expect("start");

        async fn get(port: u16, path: &str) -> String {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .expect("connect");
            s.write_all(format!("GET {path} HTTP/1.1\r\n\r\n").as_bytes())
                .await
                .expect("write");
            let mut out = String::new();
            s.read_to_string(&mut out).await.expect("read");
            out
        }

        assert!(
            get(health_port, "/readyz")
                .await
                .starts_with("HTTP/1.1 503")
        );
        obs.readiness.controllers_started();
        assert!(get(health_port, "/readyz").await.ends_with("leader"));
        let scrape = get(metrics_port, "/metrics").await;
        assert!(
            scrape.contains(r#"banlieue_leader{role="banlieue-test"} 1"#),
            "{scrape}"
        );
    }

    #[tokio::test]
    async fn observability_fails_when_the_metrics_port_is_taken() {
        let held = crate::httpd::bind(0).await.expect("bind");
        let taken = held.local_addr().expect("addr").port();
        let free = {
            let l = crate::httpd::bind(0).await.expect("bind");
            l.local_addr().expect("addr").port()
        };
        let err = start_observability("r", free, taken, crate::health::Election::Disabled)
            .await
            .expect_err("metrics port in use");
        assert!(matches!(
            err,
            BootstrapError::Bind {
                server: "metrics",
                ..
            }
        ));
    }

    #[test]
    fn provider_is_built_when_an_endpoint_is_set() {
        // Building connects to nothing: the batch exporter only sends when
        // spans are flushed, and none are recorded here.
        let provider = build_tracer_provider(
            "banlieue-controller",
            env(&[(OTEL_ENDPOINT_ENV, "http://192.0.2.10:4318")]),
        )
        .expect("build");
        assert!(provider.is_some());
        let guard = TracingGuard { provider };
        assert!(guard.exporting());
        guard.shutdown();
    }
}
