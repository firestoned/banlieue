<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0061 — `banlieue-cloud-hypervisor`: a first-party client for the VMM API

- **Status:** Proposed
- **Date:** 2026-09-25
- **Deciders:** Erick Bourgeois
- **Related:** [ADR-0060](0060-cloud-hypervisor-first-class-provider-topology.md)
  (the provider this client serves),
  [ADR-0011](0011-libvirt-provider-own-client.md) (the same call for libvirt:
  own client, no FFI, no subprocess),
  [ADR-0063](0063-cloud-hypervisor-host-supervision.md) (who starts the VMM
  whose socket this client speaks to); roadmap 09 phase 3.

## Context

Each Cloud Hypervisor guest is one `cloud-hypervisor` process serving a
REST API over a Unix socket (`--api-socket`). The provider (ADR-0060) needs
to create, boot, inspect, power off, delete and later resize, hot-unplug and
snapshot guests through it.

Options:

1. **Shell out to `ch-remote`.** Ruled out by the project rule that no
   subprocess runs in a reconcile path (ADR-0011, ADR-0050, ADR-0054): a
   CLI's stdout becomes a wire format, errors arrive as exit codes and
   free text.
2. **Generate a client from upstream's OpenAPI document**
   (`vmm/src/api/openapi/cloud-hypervisor.yaml`). The document covers about
   30 endpoints and every device type. A generator adds a build-time
   toolchain and emits types for surface banlieue must never use
   (vhost-user, vDPA, migration), which then have to be reviewed anyway.
3. **Hand-written types for the endpoints actually used**, checked against
   the upstream document. The same approach `banlieue-libvirt` takes with
   libvirt's `.x` protocol files.

The phase 0 spike (roadmap 09, 2026-09-25) against v53.0 also turned up two
defaults that are wrong for banlieue and must never be left to chance:
disk `image_type` is auto-detected (deprecated, and it disables sector-0
writes on a raw disk), and `cpus.nested` defaults to `true`.

## Decision

### 1. New crate `crates/banlieue-cloud-hypervisor`, option 3

A library crate with no Kubernetes dependency, like `banlieue-libvirt`.
HTTP/1.1 over `tokio::net::UnixStream` with `hyper` (already in the tree
through `kube`). Requests are `PUT`/`GET` on `/api/v1/<endpoint>` with JSON
bodies.

Endpoints for roadmap 09's stop condition: `vmm.ping`, `vm.create`,
`vm.boot`, `vm.info`, `vm.power-button`, `vm.shutdown`, `vm.delete`,
`vmm.shutdown`. Added by later phases: `vm.remove-device` (ADR-0065),
`vm.resize`, `vm.add-disk`, `vm.snapshot`, `vm.restore` (ADR-0066). Nothing
else gets a type until something uses it.

### 2. The upstream spec is pinned and vendored

`crates/banlieue-cloud-hypervisor/spec/cloud-hypervisor-v53.0.yaml`, copied
verbatim from the `v53.0` tag, with its sha256 in a `PIN` file beside it. A
unit test parses the vendored document and asserts that **every field the
crate serializes exists, with a compatible type, in the pinned schema**.
Upgrading the pin is a deliberate diff: new file, new digest, and whatever
the test then reports.

This needs no network in CI. It checks what matters: that banlieue's types
match the release it claims to speak.

### 3. Pure encode and decode halves, tested against captured JSON

Every endpoint has a pure `encode_*` (typed request → bytes) and `decode_*`
(bytes → typed response) half, unit-tested with no socket. Fixtures are
JSON captured from a real v53.0 VMM (the spike already has a `vm.info`),
committed with a note of where they came from. Same convention as
`banlieue-libvirt/procs.rs`.

Responses tolerate unknown fields: a newer VMM may add fields, and
refusing them would make every upstream release a breaking change.
Requests carry only fields the crate sets.

### 4. Unsafe defaults are not representable

The request builder, not the caller, sets:

- `DiskConfig.image_type = Raw` on **every** disk. There is no way to build
  a disk without it.
- `CpusConfig.nested = false`. Not a field on the builder at all: the
  environment constraint says no nested virtualization, so the option does
  not exist.
- an `RngConfig` (`/dev/urandom`) on every VM. A first boot that generates
  host keys with a starved entropy pool looks like a hang.

Each has a unit test on the encoded bytes.

### 5. Version gate

`vmm.ping` returns the VMM version. The client refuses to drive a VMM older
than the pin and returns a typed error. The provider turns that error into
a `Provider` condition, `VmmVersionUnsupported`, rather than trying anyway.

### 6. The socket is checked before it is trusted

Whoever can write to a guest's API socket owns that guest (roadmap 09,
Gotchas). Before connecting, the client checks that the socket's owner uid
and mode match what ADR-0063 creates, and refuses otherwise. That catches
a socket swapped in by another local user. The check is `stat` on the
path, so it needs no new dependency.

### 7. Errors and timeouts

Typed `thiserror` errors: transport, HTTP status with upstream's error
body, decode, version, socket ownership. Every call has a timeout. There is
no retry inside the client; retrying is the reconciler's job, where backoff
already lives.

## Consequences

**Positive**

- No subprocess and no generated code, and the surface is exactly what
  banlieue uses.
- The pin plus the schema test turns an upstream schema change into a
  failing test, not a silent field drop.
- The spike's two traps (`image_type`, `nested`) cannot come back through a
  forgotten field.

**Negative / accepted costs**

- Hand-written types must be extended by hand for each new endpoint.
  Accepted: it is a handful per phase.
- One more first-party client to maintain, alongside `banlieue-libvirt`.
- The pin has to move deliberately with upstream releases. Cloud Hypervisor
  ships often, so this is recurring, if small, work.

**Follow-ups**

- Capture fixtures for each endpoint during the spike's remaining checks.
- Add a Makefile target that re-downloads the pinned tag's spec and
  compares digests, for use when bumping the pin (not in CI).
