<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0091: Prometheus metrics on every role, recorded by one SDK reconcile wrapper

- **Status:** Accepted
- **Date:** 2026-10-08
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 12 §4.3; [ADR-0004](0004-single-binary-subcommand-dispatch.md)
  (shared bootstrap lives in the SDK); [ADR-0093](0093-health-endpoints-reflect-leadership.md)
  (the leader gauge); [ADR-0092](0092-opentelemetry-tracing-opt-in.md)

## Context

Every role of the `banlieue` binary accepts `--metrics-port`
(`BANLIEUE_METRICS_PORT`, default 8080), and every Deployment advertises it:
a `metrics` container port, `prometheus.io/scrape` annotations and a Service
port. **Nothing listens on it.** The flag is parsed and never read, so a
Prometheus configured from the annotations scrapes a closed port. The
`cloud-hypervisor` provider has no metrics flag at all.

There is also no shared reconcile wrapper. Each of the roughly twenty
`Controller::new(...).run(reconcile, error_policy, ctx).for_each(...)` call
sites repeats the same logging closure, so any instrumentation added per call
site would drift.

## Decision

1. **One metrics registry per process, served on `--metrics-port`.** The SDK
   (`banlieue-provider-sdk`) owns a [`prometheus-client`] registry and serves
   it at `GET /metrics` in the OpenMetrics text format. Any other path is a
   404. The server is started next to the health server, before leader
   election, so a standby replica is scrapeable too. `cloud-hypervisor` gains
   the same flag.

2. **Reconcile metrics come from one SDK wrapper, not from call sites.** A
   helper in `banlieue-provider-sdk` wraps `Controller::run`'s reconcile and
   error-policy functions and its result stream. Every call site moves to it.
   It records:

   | Metric | Type | Labels |
   | --- | --- | --- |
   | `banlieue_reconcile_total` | counter | `controller`, `result` (`success`, `error`, `requeue`) |
   | `banlieue_reconcile_duration_seconds` | histogram | `controller` |
   | `banlieue_reconcile_errors_total` | counter | `controller`, `kind` (the typed error's variant name) |

   `controller` is the reconciled kind (`VirtualMachine`, `VSphereMachine`,
   ...). kube-runtime's `Action` is opaque, so `result` is classified by
   comparison: a reconcile returning one of the SDK's steady-state actions
   (default or long requeue, or no requeue) is `success`, any other `Ok` is
   `requeue`, and an `Err` is `error`. No label carries an object name, namespace or free text, so
   cardinality is bounded by the number of controllers and error variants.

3. **Domain gauges are bounded too.**
   - `banlieue_leader{role}`: 1 while this process holds its lease, else 0
     (ADR-0093).
   - `banlieue_provider_failure_domains{provider,kind}`: failure domains a
     Provider reports, from its status. `provider` is the Provider's name and
     `kind` its `spec.providerClassRef.name`; both are created by an
     administrator, not tenants. The controller reads Providers through one
     reflector of its own, because the stores behind its trigger-only watches
     are not exposed by kube-runtime.
   - `banlieue_virtualmachines{phase}`: number of `VirtualMachine`s per
     phase. `VirtualMachine` status has no phase field, so the metric uses
     one derived by a fixed rule, first match wins: `Deleting` (deletion
     timestamp set), `Paused` (`spec.paused`), `Ready` (`Ready=True`),
     `Provisioning` (scheduled), otherwise `Pending`. Every phase is always
     emitted, zero included. This **replaces** the per-object
     `banlieue_vm_state{namespace,name,state}` the roadmap sketched: one
     series per VM grows with tenants, and per-object state is what
     `kubectl get` and kube-state-metrics' custom-resource support are for.
   - `banlieue_snapshot_size_bytes` waits for snapshots (roadmap 10).

4. **No authentication on `/metrics`.** It serves counts and durations, with
   no object names, credentials or spec content. Who may reach it is a
   network question, answered by the ingress rule in ADR-0094's policies.

5. **Dashboards ship as plain JSON** under `deploy/dashboards/`, built on
   these metrics only.

## Consequences

- The annotations and Service ports that already exist become true.
  Operator-spawned provider pods declare the port but have no Service or
  annotations; scraping them uses a PodMonitor or pod discovery on the
  `metrics` port, documented with the dashboards.
- Every controller call site changes once, to the wrapper. After that a new
  controller is instrumented by construction.
- `error_policy` keeps its per-module behaviour; the wrapper only observes.
- One new dependency, `prometheus-client` (maintained by the Prometheus
  project, no transitive TLS). The HTTP server is the same minimal
  hand-rolled listener the health server uses, so no web framework is added.

[`prometheus-client`]: https://github.com/prometheus/client_rust
