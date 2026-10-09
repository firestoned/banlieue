<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0093: Path-aware health endpoints; readiness means "can serve or take over"

- **Status:** Accepted
- **Date:** 2026-10-08
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 12 §4.3; [ADR-0004](0004-single-binary-subcommand-dispatch.md);
  [ADR-0091](0091-prometheus-metrics-endpoint.md) (`banlieue_leader`)

## Context

Every Deployment probes `/livez` (liveness) and `/readyz` (readiness) on the
`health` port, 8081. The server behind them,
`banlieue-provider-sdk::bootstrap::serve_health`, reads up to 1 KiB of the
request, ignores it, and answers `200 ok`. Any path, any state: it is up
before leader election and before any controller starts, and if its bind
fails it only logs, leaving the process running with no health server at
all.

The roadmap asked for health checks that "reflect leader-election state".
The obvious reading, a standby replica reports NotReady, is wrong for
banlieue. A Deployment with two replicas and a NotReady standby never
reaches its desired available count: `kubectl rollout status` waits forever,
a PodDisruptionBudget counts the standby as already disrupted, and a rolling
update can stall. controller-runtime's managers reach the same conclusion:
readiness does not depend on leadership.

## Decision

1. **The health server routes by path.** It parses the request line and
   answers:
   - `GET /livez`: 200 while the process can serve the request at all.
   - `GET /readyz`: 200 when the role can do its job, 503 otherwise, with a
     one-word body (below).
   - Anything else: 404.

2. **Ready means "serving, or able to take over".** `/readyz` is 200 when
   either:
   - this replica holds the lease and its controllers have started
     (body `leader`); or
   - it is a standby that has successfully read the current lease from the
     API server within one lease duration (body `standby`), so it would take
     over if the leader went away.

   It is 503 (body `starting` or `unreachable`) before the first lease read,
   or when the standby can no longer reach the API server. With
   `--no-leader-elect`, readiness is "controllers started".

3. **Leadership is reported, not gated on.** The `/readyz` body says `leader`
   or `standby`, and ADR-0091's `banlieue_leader{role}` gauge carries it as a
   metric. "Which replica leads" is answerable from a probe, a scrape or
   `kubectl get lease`, without making a healthy standby look broken.

4. **A health server that cannot bind is fatal.** The role exits non-zero,
   instead of running unobservable behind probes that will kill it later
   anyway.

5. **Lease loss stays fatal**, as today (`process::exit(1)` when the renewer
   fails). Liveness does not need to detect it.

6. **A standby survives a failed lease read.** Before this ADR, any error
   reading the Lease while waiting to acquire it ended the process. Now a
   standby logs it, retries on the retry period, and goes `unreachable` once
   its last successful read is older than one lease duration, which is what
   makes that state observable at all. A failure to create or patch the Lease
   while acquiring it still returns an error, as before.

## Consequences

- The paths and port in every manifest stay as they are; the answers become
  true.
- A standby that loses the API server goes NotReady and drops out of the
  Service endpoints. Nothing routes to banlieue pods except metrics and
  health, so this only makes the state visible.
- The health server keeps its hand-rolled listener, with no web framework.
  Parsing is limited to the request line, with the same 1 KiB cap, so a
  malformed or oversized request gets a 400 and the connection is closed.
