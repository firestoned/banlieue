# Guide: Observability

Every banlieue role (the controller, the operator, the image builder and each
provider) serves the same three things:

| Endpoint | Port | What it answers |
| --- | --- | --- |
| `GET /livez`, `GET /readyz` | `health`, 8081 (`--health-port`) | Liveness and readiness probes ([ADR-0093](https://github.com/firestoned/banlieue/blob/main/docs/adr/0093-health-endpoints-reflect-leadership.md)) |
| `GET /metrics` | `metrics`, 8080 (`--metrics-port`, `BANLIEUE_METRICS_PORT`) | Prometheus metrics in the OpenMetrics text format ([ADR-0091](https://github.com/firestoned/banlieue/blob/main/docs/adr/0091-prometheus-metrics-endpoint.md)) |
| OTLP traces | your collector | Off unless `OTEL_EXPORTER_OTLP_ENDPOINT` is set ([ADR-0092](https://github.com/firestoned/banlieue/blob/main/docs/adr/0092-opentelemetry-tracing-opt-in.md)) |

Both listeners start before leader election, so a standby replica answers
probes and scrapes too. If either port cannot be bound the process exits with
a non-zero status instead of running unobservable.

## Metrics

| Metric | Type | Labels | Meaning |
| --- | --- | --- | --- |
| `banlieue_reconcile_total` | counter | `controller`, `result` | Reconciles run. `result` is `success` (steady state: the default or long periodic requeue), `requeue` (the reconciler asked to run again sooner because work is in flight) or `error` |
| `banlieue_reconcile_duration_seconds` | histogram | `controller` | Wall time of one reconcile |
| `banlieue_reconcile_errors_total` | counter | `controller`, `kind` | Reconciles that returned an error. `kind` is the error's variant name (`Kube`, `Missing`, `Vsphere`, `Libvirt`, `Proxmox`, `Vmm`, ...) |
| `banlieue_leader` | gauge | `role` | `1` while this process holds its lease (or runs with `--no-leader-elect`), else `0` |
| `banlieue_virtualmachines` | gauge | `phase` | `VirtualMachine`s per phase. Controller only |
| `banlieue_provider_failure_domains` | gauge | `provider`, `kind` | Failure domains a `Provider` reports in its status; `kind` is its ProviderClass. Controller only |

`controller` is the reconciled kind: `VirtualMachine`, `VirtualMachinePool`,
`VirtualMachineClaim`, `VSphereCluster`, `VMImage`, `Provider`,
`ProviderClass`, `VSphereMachine`, `LibvirtMachine`, `ProxmoxMachine` or
`CloudHypervisorMachine`. `role` is the process's role, e.g.
`banlieue-controller` or `banlieue-provider-vsphere`.

No label carries an object name, a namespace or an error message, so the
number of series is fixed by the number of controllers and error variants, not
by how many tenants or VMs exist. Per-object state is what `kubectl get` (and
kube-state-metrics' custom-resource support) is for.

`VirtualMachine` has no `status.phase` field, so `banlieue_virtualmachines`
derives one, first match wins:

| Phase | When |
| --- | --- |
| `Deleting` | `metadata.deletionTimestamp` is set |
| `Paused` | `spec.paused` is true |
| `Ready` | the `Ready` condition is `True` |
| `Provisioning` | `status.scheduled` is set (placed, not yet Ready) |
| `Pending` | none of the above |

Every phase is always present, at zero when empty. Only the leading
controller replica reports the two controller gauges: a standby has not
started its caches.

`/metrics` has no authentication. It carries counts and durations only (no
object names, credentials or spec content); who may reach the port is a
network question, answered by your NetworkPolicy.

## Scraping

### The controller, operator and image builder

These Deployments ship with a `metrics` container port, a Service port named
`metrics`, and the conventional annotations:

```yaml
prometheus.io/scrape: "true"
prometheus.io/port: "8080"
prometheus.io/path: "/metrics"
```

A Prometheus that honours those annotations (the common
`kubernetes-pods` scrape job) picks them up with no further configuration.
With the Prometheus Operator, select the Services:

```yaml
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: banlieue
  namespace: banlieue-system
spec:
  selector:
    matchLabels:
      app.kubernetes.io/name: banlieue
  endpoints:
    - port: metrics
      path: /metrics
```

### Operator-spawned provider pods

The operator creates one provider Deployment per `Provider`. Those pods
declare the `metrics` container port but have **no Service and no scrape
annotations**, so a ServiceMonitor does not see them. Scrape them by pod: with
the Prometheus Operator, a PodMonitor on the labels every banlieue pod carries.

```yaml
apiVersion: monitoring.coreos.com/v1
kind: PodMonitor
metadata:
  name: banlieue-providers
  namespace: banlieue-system
spec:
  namespaceSelector:
    any: true          # provider workloads may run in their Provider's namespace
  selector:
    matchLabels:
      app.kubernetes.io/name: banlieue
      app.kubernetes.io/managed-by: banlieue-operator
  podMetricsEndpoints:
    - port: metrics
      path: /metrics
```

Without the Prometheus Operator, use Kubernetes pod discovery and keep the
container port named `metrics`:

```yaml
scrape_configs:
  - job_name: banlieue-providers
    kubernetes_sd_configs:
      - role: pod
    relabel_configs:
      - source_labels: [__meta_kubernetes_pod_label_app_kubernetes_io_managed_by]
        regex: banlieue-operator
        action: keep
      - source_labels: [__meta_kubernetes_pod_container_port_name]
        regex: metrics
        action: keep
      - source_labels: [__meta_kubernetes_pod_name]
        target_label: pod
```

The host-resident Cloud Hypervisor provider runs under systemd on the KVM
host, not in a pod. It serves the same endpoints on the host's
`--metrics-port` (default 8080); scrape it as a static target, for example
`bar.foo.io:8080`.

## Dashboards

`deploy/dashboards/banlieue-overview.json` is a Grafana dashboard built only on
the metrics above: reconcile rate and error ratio per controller, p50 and p95
reconcile latency, errors by kind, the leader per replica, VirtualMachines by
phase and failure domains per Provider. Import it in Grafana
(**Dashboards → New → Import**) and pick the Prometheus data source that
scrapes banlieue.

## Traces (OpenTelemetry)

Trace export is **off by default**. With no OTLP endpoint set, no exporter
starts and nothing leaves the process. Set the standard OpenTelemetry
variables on the Deployment (or the host's environment file for Cloud
Hypervisor) to turn it on:

| Variable | Effect |
| --- | --- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Turns export on; the collector base URL, e.g. `http://otel-collector.observability:4318` (`/v1/traces` is appended) |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | Turns export on; the full traces URL, used as is |
| `OTEL_EXPORTER_OTLP_HEADERS` | Extra headers, e.g. an auth token for a hosted collector |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | Export timeout in milliseconds |
| `OTEL_SERVICE_NAME` | Overrides `service.name` (default: the role, e.g. `banlieue-controller`) |
| `OTEL_RESOURCE_ATTRIBUTES` | Extra resource attributes, e.g. `deployment.environment=prod` |
| `OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG` | Sampling; the default is `parentbased_always_on` |

The transport is OTLP over HTTP/protobuf (port 4318 on a standard collector).
gRPC is not compiled in. TLS to an `https://` collector uses the same rustls
crypto provider as every other banlieue connection and the system trust store.

```yaml
env:
  - name: OTEL_EXPORTER_OTLP_ENDPOINT
    value: http://otel-collector.observability:4318
  - name: OTEL_RESOURCE_ATTRIBUTES
    value: deployment.environment=prod
```

What you get:

- a `reconcile` span for every reconcile in every role, with `controller`,
  `namespace`, `name`, `resource_version` and `result`;
- a span per backend call, named for the operation: `vsphere.clone_vm`,
  `proxmox.start`, `libvirt.domain_define`, `cloud_hypervisor.boot`, ...;
- `scheduler.schedule` and `scheduler.evaluate_migration` for placement
  decisions in the controller.

Span attributes never carry credentials, user-data, cloud-init payloads,
domain XML or Secret contents: only identifiers such as a node, a VM id, a
managed object reference or a pool name.

Turning export on gives the role egress to the collector. If you restrict
egress with NetworkPolicy, allow the collector's address yourself: it is
specific to your deployment, so banlieue cannot ship a rule for it.

## Health endpoint semantics

| Request | Answer |
| --- | --- |
| `GET /livez` | `200 ok` while the process can answer at all |
| `GET /readyz` | `200 leader` or `200 standby` when ready; `503 starting` or `503 unreachable` when not |
| any other path or method | `404` |
| a malformed request line, or one over 1 KiB | `400`, connection closed |

`/readyz` means "serving, or able to take over":

- **`leader`**: this replica holds the lease and its controllers have started.
  With `--no-leader-elect` every replica reports `leader` once its controllers
  start.
- **`standby`**: another replica holds the lease, and this one read it from the
  API server within one lease duration (15 s), so it would take over if the
  leader went away.
- **`starting`**: no lease read yet, or the lease is held but the controllers
  have not started.
- **`unreachable`**: a standby whose last successful lease read is older than
  one lease duration. It keeps retrying; it drops out of Service endpoints
  until it reaches the API server again.

### Why a standby is Ready

A NotReady standby looks like the safe choice, and it breaks the Deployment
around it. With two replicas and a NotReady standby the Deployment never
reaches its desired available count: `kubectl rollout status` waits forever, a
PodDisruptionBudget counts the standby as already disrupted, and a rolling
update can stall. controller-runtime's managers reach the same conclusion:
readiness does not depend on leadership.

So leadership is **reported**, not gated on. Which replica leads is answerable
three ways without making a healthy standby look broken:

```sh
# 1. the probe body (the image is distroless, so port-forward rather than exec)
kubectl -n banlieue-system port-forward pod/<controller-pod> 8081:8081 &
curl -s http://127.0.0.1:8081/readyz          # leader | standby

# 2. the Lease
kubectl -n banlieue-system get lease banlieue-controller

# 3. the gauge: banlieue_leader{role="banlieue-controller"} == 1
```

Losing the lease is still fatal: the leader's renewer exits the process, and
the Deployment restarts it as a candidate.
