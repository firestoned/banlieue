<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0145: A controller audits its own progress and fails liveness when objects it should reconcile go unreconciled

- **Status:** Accepted
- **Date:** 2026-11-27
- **Proposed:** 2026-11-19
- **Notes:** Not implemented.
- **Deciders:** Erick Bourgeois
- **Related:** Amends [ADR-0093](0093-health-endpoints-reflect-leadership.md) (`/livez`
  is no longer "200 while the process can answer at all"). Builds on
  [ADR-0091](0091-prometheus-metrics-endpoint.md) (the metrics endpoint and the instrumented
  reconcile wrapper every controller runs through) and [ADR-0130](0130-namespace-selector-multi-tenancy.md) (the namespace gate, whose
  unserved path is silent by design). Does not change a CRD, a token or a trust boundary.

## Context

On 2026-11-19 two `VirtualMachine`s created by the pool controller were never reconciled
by the `VirtualMachine` controller: `pool-b-vm-1` (cluster-03) and, minutes later,
`pool-a-vm-1` (cluster-01). Each had no `status`, no provider and no events for 20
and 10 minutes. The same controller pod reconciled every other VM on its usual timers the
whole time, so nothing was alarming: `/livez` and `/readyz` were green, there were no WARN or
ERROR lines, and `banlieue_reconcile_total` kept counting. A real watch event on the stuck VM
(an annotation, which bumps its resourceVersion) produced no reconcile log line either.
Restarting the pod (`rollout restart deploy/banlieue-controller`) fixed those two VMs at
once: both were reconciled in the first second and registered as nodes about 40 seconds
later.

**It did not fix the controller.** The restarted pod (started 15:48:20) went blind to
`VirtualMachine`s again within about a minute. Two VMs created after it started, about 15:50
(`pool-a-vm-2` and `pool-b-vm-2`), were never reconciled and were deleted by
the pool for `ProvisioningTimeout` at 16:19 and 16:20, and the two that replaced them were
ignored too. At 16:22 the pod's own `banlieue_virtualmachines` gauge counted 79 VMs (77 `Ready`
and 2 `Pending`) against 81 in the API: the 77 that were `Ready` at start, the 2 that were
`Pending` at start and have since become `Ready` (so the cache still shows them `Pending`),
and none of the VMs created afterwards. So the cache holds the initial list and nothing after
it. At 16:17 the claim controller, which also watches `VirtualMachine`s through `owns`, logged
`too old resource version: 151961361 (151990795): Expired`: the resourceVersion it held dated
from about 15:49, so that watch had received nothing, not even bookmarks, for 28 minutes, while
an external `kubectl get virtualmachines -A --watch-only` showed events immediately. The API
server is `v1.35.1+k0s` and its watch works from outside the cluster; the controller, the
operator and the vSphere provider run on the same node (`node-a`) behind Calico and
kube-proxy with no network policy, and the provider's watches are fine.

This corrects the account above of a controller that "stopped" after hours: the fault
reproduces on a fresh process, about a minute in, for the `VirtualMachine` watch.

What was established, from the logs, the cluster and the code (kube-runtime and kube-client
5.0.0, the version of the running v0.5.10 image and of `main`):

- The pool controller created the members (`creating pool member`), so the objects exist and
  are visible to the API. The vSphere provider was `Ready` and `Alive`. The failure domain
  was healthy in banlieue's inventory, and a different domain behaved the same way.
- The VM controller never **logged** a reconcile for either VM. Both outcomes "never
  invoked" and "invoked, then returned early" look identical in the logs: the `reconcile`
  span is created before any event, and `NamespaceGate::skip_unserved` returns
  `Action::await_change()` (no timer, no log line) before the first `info!`.

What was ruled out:

- **No client timeouts.** `build_client_with` sets `read_timeout` (295 s) and
  `write_timeout` (30 s), and kube-runtime 5.0.0's watcher has its own idle timeout
  (server timeout plus a margin) that reconnects a dead watch.
- **A transient empty namespace gate.** The reflector store swaps its buffer atomically on
  `InitDone`, so a relist never shows an empty store, and `banlieue-system` has carried
  `banlieue.io/tenant=true` since 2026-11-16, so the old VMs in that namespace were served all
  along.
- **Runner starvation.** `Controller::run` has unbounded concurrency by default.
- **vSphere HA.** It is a real event in cluster-03 but cannot stop a reconcile from starting.

**The cause, found the same day: an API server whose watch for one resource delivers nothing.**
The cluster has three control-plane API servers (`node-a`, `03` and `05`) behind the
kube-proxy service address, and each new watch connection from a pod lands on one of them.
Watching directly against each, from outside the cluster, for 18 to 25 seconds at a time
(repeated, stable across rounds and over several minutes):

| API server | `virtualmachines` watch | `leases` watch | `providers` watch |
| --- | --- | --- | --- |
| `node-a` | events | events | events |
| `node-b` | **0 events, no error** | events | events |
| `node-c` | events | **0 events, no error** | events |

`/livez` and `/readyz` are `ok` on all three, and a plain `LIST` of VMs returns the same 77
objects on all three. So a watch on `node-b` for `VirtualMachine` is accepted and then
never delivers an event, with no error, while the same watch on `01` or `05` delivers about
ten a second. A stream that lands there is blind for as long as its connection lives
(hyper keeps the pooled connection, so a re-watch after the 290 s timeout goes back to the
same server). A restart opens fresh connections, which re-rolls the dice per stream: the
first restart's pod went blind within a minute, and a later pod's main stream landed on a
healthy server. In that later pod's debug log (`RUST_LOG=kube_client=debug`) the watches that
re-established after 290 s resumed from two different resourceVersions: the streams on a
healthy server from a current one (`15201617x`), and others from the one the pod started with
(`15200801x`): no events and no bookmarks in 290 s on a resource that changes ten times a
second.

That explains every symptom: the controller's cache is the initial list and nothing after it,
the claim controller's stale resourceVersion expired (`too old resource version`), the
`VirtualMachine` controller kept reconciling what it already had on its timers, no log line,
no probe and no metric noticed, and an external `kubectl` happened to use a healthy server.
It is **not a banlieue bug**: it is an API-server-side fault (a per-resource watch cache that
stopped following storage on one control-plane node). Why those caches stopped, and whether
the storage layer (the cluster's `etcd ok` may be a shim) is involved, is not established
here; the fix is on the cluster (see Open). What is banlieue's: a controller cannot tell that
one of its watches is dead, and a dead watch looks exactly like an idle one.

An earlier note that the controller's cache was "about 9 hours stale", inferred from
resourceVersions in its log, was wrong: an object's resourceVersion is its own last-modified
marker, not a cache timestamp.

**Diagnosis on the node (2026-11-19), and the bounce that was enough.** The API servers export
their own counters: `apiserver_watch_cache_resource_version` (the newest revision each watch
cache has seen) shows the two stalled caches frozen far behind their peers, `virtualmachines`
on `node-b` at 149398699 and `leases` on `node-c` at 147065918, against a cluster at
about 152050980. A scan of all 225 watch caches on all three nodes found no other stalled
cache (two idle CRDs differ only because they have no events). On `node-b` the API server
logged, at 02:30:13 UTC on 2026-11-18, `Cache consistency check failed` for `virtualmachines`
at exactly `resourceVersion=149398699`, with different etcd and cache digests, and then
`too old resource version` every 6 minutes, 372 times. The same checker has recorded 317 to 433
digest mismatches on each node over a 15-day uptime, so divergence is routine here and almost
always heals. The datastore is kine v0.14.12 over a four-host PostgreSQL with failover, one
kine process per control-plane node, and its metrics and log show a database that is far from
quiet: server-side terminations (`57P01`), LDAP login failures, "remaining connection slots
are reserved", connection timeouts to the primary during failover, queries of 5 to 20 seconds,
13,129 failed inserts (mostly duplicate-key conflicts), and one `relation "key_value" does not
exist`.

Restarting **only kine** on `node-b` (a `SIGQUIT`, which makes it print a goroutine dump;
k0s restarted it in 2 seconds, with the API server untouched) healed the frozen cache within
seconds: the watch went from 0 to about 90 events per 15 seconds and the cache revision caught
up. So the stall was in the path between kine and the cache, not in the cache's own state, and
a kine restart is a sufficient and much smaller bounce than restarting the API server.

What kine's source (v0.14.12) says about that path: one poller feeds a `Broadcaster`; each etcd
watch is a subscriber with a 100-slot buffer, and when a subscriber's buffer is full the
broadcaster **drops the event and unsubscribes the subscriber** (`select { case sub <- item:
default: // Slow consumer, drop; go b.unsub(sub, true) }`); `LogStructured.Watch` then does a
blocking send into a second 100-slot channel that feeds the gRPC stream. The two resources that
went blind are the two busiest in the cluster (leases and virtual machines, several events a
second each), which is where a consumer stall of a few seconds overflows 100 slots. kine says it
sends `Canceled=true` to the client when a watch ends, which should make the API server re-watch;
that it did not recover here is **not explained**. The goroutine dump (taken after the stall) shows
no goroutine blocked in a send, and the ones that exist are subscribers waiting on events. The
exact trigger is therefore still an inference from the code and the two resources' event rates,
not something observed.

## Decision

1. **Every audited controller records which objects it has been invoked for.** The
   instrumented reconcile wrapper ([ADR-0091](0091-prometheus-metrics-endpoint.md)) adds
   the object's UID to a per-controller **ledger** on every invocation, including an early
   return. Only controllers that opt in with `Metrics::enable_audit(controller)` keep a ledger,
   so an un-audited controller's memory does not grow with object churn.

2. **An audit loop compares the API, the cache and the ledger.** Every 60 s, for an audited
   controller, it lists the kind from the API server authoritatively (metadata only,
   paginated, over the controller's served scope) and, for each object older than a **grace**
   (120 s), reports:
   - `missing_from_store`: the object exists in the API but not in the controller's
     reflector store (the cache is not delivering it);
   - `never_reconciled`: it is in the store but the controller has never been invoked for its
     UID (the work never reaches the reconcile).

   An object in a namespace the gate does not serve is **excluded**: not reconciling it is the
   point of the gate. An object being deleted is included (its finalizer must still run).
   Ledger entries for UIDs that no longer exist are dropped on each pass. The loop does not
   start before the process has been up for 180 s, so a freshly restarted controller, which
   reconciles everything at once, never trips it.

3. **What it reports.** `banlieue_audit_unreconciled_objects{controller,reason}` is a gauge
   (label values are code constants, never object names). The first pass that sees a problem
   logs `WARN` with up to 20 object names and the counts (API, store, ledger) and the oldest
   age; recovery logs `INFO`.

4. **Sustained failure fails liveness.** If an audited controller has reported at least one
   unreconciled object on every pass for **300 s**, `/livez` answers `503` with the body
   `stale` (the controller's name goes to the log, not the body), and the kubelet restarts the
   pod after the probe's failure threshold. The restart is the recovery. The pass that crosses
   the threshold logs `ERROR` with the same evidence as decision 3, so the occurrence leaves a
   record that survives the restart. A single transient gap (an object created seconds ago, a
   slow reconcile) never reaches 300 s.

5. **The `VirtualMachine` controller opts in now.** It is the controller that failed. The
   mechanism is generic, and a further controller opts in with one call and one `spawn`.

6. **The silent skip gets a log line.** The unserved-namespace early return in the
   `VirtualMachine` reconcile logs at `debug` with the namespace and object. It stays
   silent at `info` (a tenant outside the selector is normal), but "invoked and skipped" is now
   distinguishable from "never invoked" when debug logging is on.

## Consequences

- A controller that silently stops acting on objects is noticed within about 2 to 3 minutes,
  and recovered by a restart about 5 to 6 minutes after the objects appear (grace 120 s, then
  300 s of sustained failure, then 3 failed probes at 30 s). Before this, it lasted until a
  person looked.
- **Liveness now depends on the controller's own behavior.** A controller that can never reach
  a state in which every served object has been invoked would restart forever. The audit only
  flags objects the controller has never been invoked for, and invocation does not depend on
  the reconcile succeeding, so a failing reconcile (an error, a refused pairing) does not trip
  it. A namespace the gate does not serve is excluded.
- One extra authoritative `list` (metadata only) per minute per audited controller.
- **A safety net, not the fix.** The cause is an API server whose watch cache is blind, which
  banlieue cannot repair. While one stays blind, a restart re-rolls which server each stream
  lands on, so each restart has a chance (with one bad server of three, about 2 in 3 for the
  `VirtualMachine` stream) of landing on healthy ones, and the audit restarts the pod about
  every 9 to 10 minutes until it does (180 s start-up grace, then 300 s of bad passes, then 3
  probes at 30 s). A VM created in a blind window waits for that restart. That is far better
  than never, and the log it writes names the objects, but it does not replace repairing the
  API server. The thresholds are this ADR's numbers and can be tightened.
- `/livez` can now be `503`. A probe that treats any non-200 as dead (the default) restarts the
  pod, which is the intent; a deployment with no liveness probe gets the gauge and the log but
  no restart.
- The audit would have named this fault on the day. Its log gives the objects, the three counts
  and the age, which separates a cache that is not delivering (`missing_from_store`, what a
  blind watch produces) from a scheduler that never invokes (`never_reconciled`). That is the
  evidence that was missing on 2026-11-19, and it was found by hand with direct per-server
  watches instead.
- Threat model: no new trust boundary, credential or identity. An actor who can create objects
  in a served namespace cannot make the controller restart, because creating an object the
  controller is invoked for (even if every reconcile errors) satisfies the audit; only a fault
  in the controller itself, or in what it is delivered, can.

## Alternatives considered

- **Rely on the library's idle timeout.** Already in place (client and watcher) and did not
  prevent this incident: a blind watch looks exactly like an idle one, and the re-watch reuses
  the pooled connection to the same server. Kept; this adds a check on the outcome, not the
  mechanism.
- **A scheduled restart (a CronJob or a short pod lifetime).** Bounds the damage without
  naming it, restarts a healthy controller for nothing, and leaves no evidence.
- **An extra list on every reconcile to double-check the store.** Costs an API call per
  reconcile, per object, forever, to cover a rare fault.
- **Replace the reflector with periodic relisting.** Throws away event-driven reconciles to
  cover a fault whose cause is not yet known.
- **A readiness failure instead of a liveness failure.** A not-ready controller keeps running
  and keeps not reconciling; only a restart recovers it.

## Open

- **Repair the API servers.** `node-b` is repaired (kine restarted 2026-11-19 17:22 UTC).
  `node-c`'s `Lease` watch is still blind and needs the same: signal its kine, re-run the
  per-server watch test. Why the caches stop is not established, and it may recur; until the
  datastore is steadier every client of the busiest resources in the cluster is at risk, not
  only banlieue.
- **A watch that survives a blind server.** Whether banlieue should, on a stream that has
  delivered neither an event nor a bookmark for a few minutes, force a fresh connection so the
  next attempt can land on another server, instead of relying on the pooled one.
- **The VirtualMachine controller writes every VM on every reconcile.** An idle, `Ready` VM
  changes in exactly one field between samples, `status.scheduledAt`
  (`reconciler/virtualmachine.rs`, `next_status.scheduled = Some(decision.to_scheduled_placement(
  Timestamp::now()))`), which the API documents as the time the placement decision was made. Each
  write is a new revision and a watch event on all three nodes, and the controller reconciles on
  its own status updates, so it is a feedback loop: about 25 `VirtualMachine` events a second
  across about 80 VMs, against about 10 a second for all `Lease`s. It is the busiest resource
  in the cluster and the one whose watch went blind. **Fixed in the controller** (not yet
  rolled out): `Decision::to_scheduled_placement_keeping` keeps the previous `scheduledAt` while
  the placement is unchanged (a first placement, a changed one, or a status that never recorded
  a time are stamped now), so a reconcile of an idle VM writes nothing new. Measured before the
  fix on 2026-11-19: 417 `VirtualMachine` events in 30 s across 77 VMs, one VM rewritten about
  every 4 s with `status.scheduledAt` the only field that changed. After the rollout the same
  count should fall to the VMs that really change; re-run the 30 s watch to confirm. Skipping a
  status patch that changes nothing is a further, smaller saving not made here (an identical
  server-side apply does not create a revision).
- **kine settings.** The pool is already tuned through `KINE_KINE_DATASTORE_MAX_IDLE_CONNECTIONS=10`
  and `KINE_KINE_DATASTORE_MAX_OPEN_CONNECTIONS=20` on the `k0scontroller` unit (k0s strips the
  first `KINE_`), but idle (10) is below open (20), so connections above ten are closed after
  every burst: 405,678 closes in 5.3 days on one node, each reopening costs a TLS and an LDAP
  login to the database. See the analysis for the options and their limits; the 100-slot
  subscriber buffers in kine are not configurable.
- **A cluster-side check.** A probe that watches each API server for a resource that changes
  constantly (`Lease`s, `VirtualMachine`s) and alerts on silence would have caught this with no
  banlieue code at all.
- Whether the pool, claim and `VMImage` controllers should opt in. They are one line each once
  the audit has run clean on the `VirtualMachine` controller for a while.
