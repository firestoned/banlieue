// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::health`]: the router as a pure function of
//! (request bytes, readiness), and the readiness state transitions.

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use prometheus_client::metrics::gauge::Gauge;

    use super::super::*;
    use crate::httpd::Status;

    const LEASE: Duration = Duration::from_secs(15);

    fn elected() -> Election {
        Election::Enabled {
            lease_duration: LEASE,
        }
    }

    fn snapshot(is_leader: bool, started: bool, last: Option<Instant>) -> ReadinessSnapshot {
        ReadinessSnapshot {
            election: elected(),
            is_leader,
            controllers_started: started,
            last_lease_read: last,
        }
    }

    // ---- router -----------------------------------------------------------

    #[test]
    fn livez_is_200_ok_in_every_state() {
        for state in [
            ReadyState::Leader,
            ReadyState::Standby,
            ReadyState::Starting,
            ReadyState::Unreachable,
        ] {
            let response = handle(b"GET /livez HTTP/1.1\r\n\r\n", state);
            assert_eq!(response.status, Status::Ok);
            assert_eq!(response.body, "ok");
        }
    }

    #[test]
    fn readyz_is_200_with_role_body_when_ready() {
        let leader = handle(b"GET /readyz HTTP/1.1\r\n\r\n", ReadyState::Leader);
        assert_eq!(
            (leader.status, leader.body.as_str()),
            (Status::Ok, "leader")
        );

        let standby = handle(b"GET /readyz HTTP/1.1\r\n\r\n", ReadyState::Standby);
        assert_eq!(
            (standby.status, standby.body.as_str()),
            (Status::Ok, "standby")
        );
    }

    #[test]
    fn readyz_is_503_with_reason_body_when_not_ready() {
        let starting = handle(b"GET /readyz HTTP/1.1\r\n\r\n", ReadyState::Starting);
        assert_eq!(
            (starting.status, starting.body.as_str()),
            (Status::ServiceUnavailable, "starting")
        );

        let unreachable = handle(b"GET /readyz HTTP/1.1\r\n\r\n", ReadyState::Unreachable);
        assert_eq!(
            (unreachable.status, unreachable.body.as_str()),
            (Status::ServiceUnavailable, "unreachable")
        );
    }

    #[test]
    fn unknown_paths_are_404() {
        for raw in [
            &b"GET / HTTP/1.1\r\n"[..],
            b"GET /healthz HTTP/1.1\r\n",
            b"GET /readyz/extra HTTP/1.1\r\n",
            b"GET /metrics HTTP/1.1\r\n",
        ] {
            assert_eq!(handle(raw, ReadyState::Leader).status, Status::NotFound);
        }
    }

    #[test]
    fn non_get_methods_are_404() {
        assert_eq!(
            handle(b"POST /readyz HTTP/1.1\r\n", ReadyState::Leader).status,
            Status::NotFound
        );
    }

    #[test]
    fn malformed_and_oversized_requests_are_400() {
        assert_eq!(
            handle(b"not http at all\r\n", ReadyState::Leader).status,
            Status::BadRequest
        );
        let oversized = vec![b'G'; crate::httpd::MAX_REQUEST_LINE_BYTES];
        assert_eq!(
            handle(&oversized, ReadyState::Leader).status,
            Status::BadRequest
        );
    }

    // ---- evaluate ---------------------------------------------------------

    #[test]
    fn candidate_before_first_lease_read_is_starting() {
        let now = Instant::now();
        assert_eq!(
            evaluate(&snapshot(false, false, None), now),
            ReadyState::Starting
        );
    }

    #[test]
    fn standby_within_one_lease_duration_is_ready() {
        let now = Instant::now();
        let read = now.checked_sub(LEASE).expect("monotonic clock past lease");
        assert_eq!(
            evaluate(&snapshot(false, false, Some(read)), now),
            ReadyState::Standby
        );
    }

    #[test]
    fn standby_past_one_lease_duration_is_unreachable() {
        let now = Instant::now();
        let read = now
            .checked_sub(LEASE + Duration::from_secs(1))
            .expect("monotonic clock past lease");
        assert_eq!(
            evaluate(&snapshot(false, false, Some(read)), now),
            ReadyState::Unreachable
        );
    }

    #[test]
    fn leader_is_starting_until_controllers_start() {
        let now = Instant::now();
        assert_eq!(
            evaluate(&snapshot(true, false, Some(now)), now),
            ReadyState::Starting
        );
        assert_eq!(
            evaluate(&snapshot(true, true, Some(now)), now),
            ReadyState::Leader
        );
    }

    #[test]
    fn leader_does_not_age_out_on_lease_reads() {
        // A leader's own renewer exits the process on lease loss; readiness
        // does not second-guess it (ADR-0093 Decision 5).
        let now = Instant::now();
        let stale = now
            .checked_sub(LEASE * 2)
            .expect("monotonic clock past lease");
        assert_eq!(
            evaluate(&snapshot(true, true, Some(stale)), now),
            ReadyState::Leader
        );
    }

    #[test]
    fn without_election_ready_means_controllers_started() {
        let now = Instant::now();
        let mut s = ReadinessSnapshot {
            election: Election::Disabled,
            is_leader: false,
            controllers_started: false,
            last_lease_read: None,
        };
        assert_eq!(evaluate(&s, now), ReadyState::Starting);
        s.controllers_started = true;
        assert_eq!(evaluate(&s, now), ReadyState::Leader);
    }

    // ---- Readiness transitions -------------------------------------------

    #[test]
    fn readiness_walks_starting_standby_leader_and_drives_the_gauge() {
        let gauge = Gauge::default();
        let readiness = Readiness::new(elected(), gauge.clone());
        assert_eq!(readiness.state(), ReadyState::Starting);
        assert_eq!(gauge.get(), 0);

        readiness.lease_observed();
        assert_eq!(readiness.state(), ReadyState::Standby);
        assert_eq!(gauge.get(), 0);

        readiness.became_leader();
        assert_eq!(readiness.state(), ReadyState::Starting);
        assert_eq!(gauge.get(), 1);

        readiness.controllers_started();
        assert_eq!(readiness.state(), ReadyState::Leader);

        readiness.lost_leadership();
        assert_eq!(gauge.get(), 0);
        assert_eq!(readiness.state(), ReadyState::Standby);
    }

    #[test]
    fn readiness_reports_unreachable_after_stale_read() {
        let readiness = Readiness::new(elected(), Gauge::default());
        let stale = Instant::now()
            .checked_sub(LEASE * 2)
            .expect("monotonic clock past lease");
        readiness.lease_observed_at(stale);
        assert_eq!(readiness.state(), ReadyState::Unreachable);
    }

    #[test]
    fn readiness_without_election_sets_gauge_when_controllers_start() {
        let gauge = Gauge::default();
        let readiness = Readiness::new(Election::Disabled, gauge.clone());
        assert_eq!(readiness.state(), ReadyState::Starting);
        readiness.controllers_started();
        assert_eq!(readiness.state(), ReadyState::Leader);
        assert_eq!(gauge.get(), 1);
    }

    #[test]
    fn clones_share_state() {
        let readiness = Readiness::new(Election::Disabled, Gauge::default());
        let probe_side = readiness.clone();
        readiness.controllers_started();
        assert_eq!(probe_side.state(), ReadyState::Leader);
    }

    #[tokio::test]
    async fn health_server_bind_failure_is_an_error() {
        let held = crate::httpd::bind(0).await.expect("bind ephemeral port");
        let port = held.local_addr().expect("addr").port();
        let err = start_health_server(port, Readiness::new(Election::Disabled, Gauge::default()))
            .await
            .expect_err("port already bound");
        assert!(matches!(err, BootstrapError::Bind { port: p, .. } if p == port));
    }
}
