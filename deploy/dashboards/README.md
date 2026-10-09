<!-- Copyright (c) 2026 Erick Bourgeois, banlieue -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# banlieue Grafana dashboards

Plain Grafana dashboard JSON (ADR-0091 Decision 5), built only on the
`banlieue_*` metrics every role serves at `GET /metrics` on its `metrics`
port (8080).

| File | Shows |
| --- | --- |
| `banlieue-overview.json` | Reconcile rate, error ratio, p50/p95 latency and errors by kind per controller; the leader per replica; VirtualMachines by phase; failure domains per Provider |

Import it in Grafana (**Dashboards → New → Import**, upload the file) and pick
the Prometheus data source that scrapes banlieue. The `Job` variable lists
every scrape job that reports `banlieue_leader`.

How to get the metrics into Prometheus, including the operator-spawned
provider pods that have no Service, is in the observability guide
(`docs/src/guides/observability.md`).
