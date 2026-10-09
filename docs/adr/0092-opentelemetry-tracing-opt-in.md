<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0092: OpenTelemetry trace export, opt-in through the standard environment

- **Status:** Accepted
- **Date:** 2026-10-08
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 12 §4.3; D-017 in `.github/community/01-decisions.md`
  ("every reconcile is a span"); [ADR-0091](0091-prometheus-metrics-endpoint.md)

## Context

Logs go through `tracing`, initialised once in
`banlieue-provider-sdk::bootstrap::init_tracing` (text or JSON). kube-runtime
already opens a span per reconcile. But there is no exporter, so spans never
leave the process, and backend calls (vCenter, Proxmox, libvirt, Cloud
Hypervisor) have no spans of their own. A slow `VirtualMachine` cannot be
followed from the controller through the provider to the hypervisor call that
made it slow.

## Decision

1. **Export with OTLP, off by default.** `init_tracing` adds a
   `tracing-opentelemetry` layer only when `OTEL_EXPORTER_OTLP_ENDPOINT` (or
   `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`) is set. With neither set, nothing
   changes and no exporter thread starts. The standard `OTEL_*` variables
   (service name, sampler, headers, protocol) are honoured as the
   OpenTelemetry SDK defines them; banlieue adds no flags of its own.

2. **OTLP over HTTP/protobuf, through the workspace's existing reqwest and
   rustls stack.** Not gRPC, which would add tonic and a second HTTP/2 stack.
   The exporter uses the process's installed rustls crypto provider, so TLS
   to a collector inherits whatever the rest of banlieue uses.

3. **`service.name` is the role** (`banlieue-controller`,
   `banlieue-provider-vsphere`, ...) unless `OTEL_SERVICE_NAME` overrides it.
   `service.version` is the binary version.

4. **Spans:**
   - one per reconcile, from the ADR-0091 wrapper, with `controller`,
     `namespace`, `name`, `resource_version` and `result`, so D-017 holds in
     every role;
   - one per backend API call, named for the operation (`vsphere.clone_vm`,
     `proxmox.start`, `libvirt.domain_define`, ...), on each provider
     client's public operations;
   - scheduling and placement decisions in the controller.

   Span attributes never carry credentials, user-data, Secret contents or
   cloud-init payloads.

5. **Sampling defaults to parent-based always-on.** The rate is the
   operator's choice through `OTEL_TRACES_SAMPLER`.

## Consequences

- Off by default: no new egress, no collector dependency, no change for
  anyone who does not set the variable.
- When on, a role gains egress to the collector. ADR-0094's policies leave
  that to the operator, because the collector's address is deployment
  specific.
- New dependencies: `opentelemetry`, `opentelemetry_sdk`,
  `opentelemetry-otlp` (http-proto) and `tracing-opentelemetry`, all from the
  OpenTelemetry project.
- TLS to a collector uses the same crypto provider as every other banlieue
  connection, so its key exchange is only as post-quantum-ready as that
  provider (today `ring`, classical X25519). Moving the workspace to a
  provider with hybrid post-quantum key exchange is a separate, cross-cutting
  decision that covers this exporter too.
