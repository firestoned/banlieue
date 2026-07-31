<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0011 — `banlieue-provider-libvirt`: own the libvirt client, zero new dependencies

- **Status:** Accepted
- **Date:** 2026-07-30
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0010 (VMImage build pipeline); ADR-0004 (single-binary
  subcommand dispatch); ADR-0003 (provider deployment topology); ADR-0008/0009
  (the vSphere transport, whose BYOC/rustls posture this reuses).
  Supersedes nothing.

> **Revised before implementation (2026-07-30).** The first draft of this ADR
> decided "no client library; run `virsh` inside Jobs for everything." That was
> rejected on review for two reasons: it made a CLI's stdout a wire format, and
> it needed a Job per `Provider` probe just to list pools. The decision below
> replaces it. The rejected option is retained under *Alternatives considered*
> because the reasoning that killed it (bulk data must not flow through the
> controller) still governs the data path.

## Context

banlieue ships one working provider, `banlieue-provider-vsphere`. The
project's reference development environment has **no vSphere at all** — it is
a single libvirt/KVM host running the k0s cluster itself as guests. Every
provider-side code path is therefore currently untestable on the hardware that
exists, and ADR-0010's pipeline stops at a raw disk in a PVC with nothing able
to consume it.

This ADR covers the **first increment** of a libvirt provider: register a
libvirt host as a `Provider`, and import the raw disk that
`banlieue-imagebuilder` produces into that host's storage pools. VM lifecycle
(a `LibvirtMachine` InfraMachine CRD, create/power/delete) is explicitly **out
of scope** and deferred to a later ADR.

### The environment, as measured

Read off the reference libvirt host directly rather than assumed:

- libvirt **11.3.0**, `qemu:///system`.
- Storage pools: `boot`, `default`, `k0s-bootstrap` (all running, all
  directory-backed on one 432 GiB filesystem), plus `images` (inactive).
- Networks: `default` only.
- `virsh vol-upload` is available and supports `--sparse`.

Note the shape difference from vSphere: there is no datacenter/cluster
hierarchy to walk. A libvirt "failure domain" is essentially *the host*, and
the interesting sub-structure is which **pool** a volume lands in.

### How does Rust talk to libvirt?

Surveyed against the project's dependency rule (actively maintained, prefer
well-known crates):

| Crate | Version | Last publish | Verdict |
| --- | --- | --- | --- |
| `virt` | 0.4.3 | 2025-08 | Official (gitlab.com/libvirt/libvirt-rust), 270k downloads, **but** FFI to the libvirt C library, and ~11 months stale |
| `virt-sys` | 0.3.1 | 2025-08 | Raw FFI layer beneath `virt`; same constraints |
| `libvirt` | 0.1.0 | **2015** | Abandoned |
| `libvirt-rpc` | 0.1.12 | **2018** | Pure-Rust wire protocol, abandoned |

`virt` *does* expose what we would need (`StorageVol::upload(&Stream, ...)`,
`create_xml`, `get_info`), so it is technically viable. There is no maintained
pure-Rust alternative.

### Why the obvious choice is still the wrong one

Adopting `virt` would mean:

1. **A C library in the runtime image.** banlieue ships distroless and
   Chainguard images and has gone to real trouble to stay OpenSSL-free and
   cross-compilable from an arm64 macOS host with a plain gcc toolchain
   (ADR-0009). Linking `libvirt0` reintroduces exactly the class of native
   dependency that was deliberately removed, and complicates the
   arm64-host → amd64-target cross build.
2. **A stale dependency** on a project the rules would otherwise reject.
3. **It would make a control-plane component a data-plane one.** Importing a
   guest image means moving multiple gigabytes. Streaming that through a
   reconcile loop is wrong on every axis — memory footprint, what a
   mid-transfer pod restart means, how requeue interacts with a half-written
   volume, and the fact that a controller blocked for minutes on I/O stops
   reconciling everything else.

Point 3 is not a libvirt-specific insight and survives into the decision
below: ADR-0010 already committed to the same separation for vSphere, where
per-zone import "mounts the shared artifacts PVC read-only" in a **Job**
rather than in the provider process.

### How small is the protocol, actually?

Measured rather than assumed, from libvirt's own RPC documentation:

- A 32-bit length prefix, then a **fixed 24-byte header** (program, version,
  procedure, type, serial, status), then an XDR-encoded payload.
- XDR (RFC 4506) is big-endian and 4-byte aligned — a few hundred lines for
  the handful of types we use.
- **Stream packets carry raw bytes with no XDR encoding**, so the bulk path is
  simpler than the control path, not harder.

The reference libvirt host, inspected directly, exposed plaintext TCP 16509
with `auth_tcp = "sasl"` and `mech_list: digest-md5`. That combination is a
poor foundation: RFC 6331 declared DIGEST-MD5 obsolete, and absent a
negotiated SASL security layer the session — including every byte of an
uploaded disk image — is unencrypted. Switching the host to TLS is therefore
both the security fix and a *simplification*: with libvirt's default
`auth_tls = "none"`, the **x509 client certificate is the credential**, so
there is no SASL exchange to implement and no MD5 anywhere.

## Decision

**Write a minimal libvirt client of our own, `crates/banlieue-libvirt`, in
pure Rust with no new third-party dependencies. Keep bulk data transfer out of
the controller process.**

Dependency budget, following the project's "keep dependencies minimal; vendor
anything under ~500 lines" rule:

| Component | Approx. size | Dependency |
| --- | --- | --- |
| XDR codec | ~250 lines | **none** — written here (both crates.io options are abandoned: 2017 and 2020) |
| RPC framing + session | ~200 lines | **none** — written here |
| Procedure definitions (~8) | ~300 lines | **none** — written here |
| Transport | ~100 lines | `rustls` + `tokio` — **already workspace dependencies** (ADR-0008/0009) |

Net new dependencies: **zero**. No C library, no FFI, no abandoned crate, and
the distroless/Chainguard images and macOS→Linux cross build are untouched.

### Transport: TLS only

`qemu+tls://host/system` (port 16514) with mutual TLS. The client certificate
authenticates; there is no password, no SASL, no MD5. `rustls` is already
pinned in the workspace for the vSphere BYOC work, so the same crypto posture
(ring provider, no OpenSSL) carries over unchanged.

Plaintext TCP 16509 is **not supported by this provider**, even though the
reference host had it enabled. Supporting it would mean implementing SASL
DIGEST-MD5 purely to talk to an obsolete, unencrypted transport — more code
for a worse security posture.

`scripts/bootstrap-libvirt-tls.sh` provisions the PKI (CA, server cert with
SANs covering every address the host answers on, client cert), flips
`listen_tls = 1` / `listen_tcp = 0`, and emits the client credentials as a
Kubernetes Secret manifest. The SAN breadth is deliberate: libvirt validates
the server certificate against *the address the client dialled*, and the
libvirt bridge address (`virbr0`) — which is how in-cluster workloads reach
the host — is the one most easily forgotten.

### Control plane in-process, data plane in a Job

- **`Provider` reconciler** talks to libvirtd directly through
  `banlieue-libvirt`: connect, list pools, list networks, verify the
  admin-declared `spec.capabilities` actually exist, publish
  `status.failureDomains[]`. These are two cheap list calls, so no Job and no
  probe-scheduling gymnastics — the first draft's Job-per-probe compromise
  disappears entirely.
- **`VMImage` import** runs as a Kubernetes Job, for the reason that killed
  option 3 above. The Job runs **the `banlieue` binary itself** (a dedicated
  subcommand) rather than a third-party `virsh`/`qemu-img` image, so there is
  no external image to pin, trust, or keep patched, and the same
  `banlieue-libvirt` code is exercised in both roles.

### Volume format: raw, for now

The first increment uploads the artifact **as a raw volume**, exactly as
`banlieue-imagebuilder` produced it. This removes `qemu-img` — and therefore
the last reason to need a tools image — from the pipeline entirely.

The cost is allocated size: a raw volume consumes its full extent unless holes
are skipped. libvirt's stream protocol supports sparse transfer, so
hole-skipping is a follow-up optimisation within our own crate rather than a
reason to shell out to `qemu-img`. qcow2 conversion can be added later if
image size becomes a real constraint.

### Crate shape

Two new crates:

- **`crates/banlieue-libvirt`** — the client. Pure protocol: XDR codec, RPC
  framing, TLS transport, the procedures we use, and streams. No `kube`
  dependency, no banlieue types; it could be published standalone.
- **`crates/banlieue-provider-libvirt`** — the controller, following ADR-0004:
  `pub struct Cli` (`clap::Args`) + `pub async fn run(cli) -> anyhow::Result<()>`,
  built on `banlieue-provider-sdk` (`bootstrap`, `leader`, `ssa`, `status`,
  `reconciler`). Dispatched as `banlieue provider libvirt`, gated behind a
  default-on `libvirt` Cargo feature. Field manager:
  `banlieue.io/provider-libvirt` (already reserved in
  `banlieue-provider-sdk::ssa`).

Keeping the protocol crate free of `kube` is what lets the import Job reuse it
without dragging the controller's dependencies into the data path.

### Two reconcilers

**`Provider`** (class `libvirt`) — connects over TLS and verifies that the
pools and networks the admin declared in `spec.capabilities` actually exist,
then publishes `status.failureDomains[]`. Two list calls in-process; no Job.

Capabilities remain **declared, not discovered** (non-negotiable #4): the
provider *verifies* the admin's declaration and reports which entries are
actually present, exactly as the vSphere provider narrows
`FailureDomainAttributes.available_storage_classes`. One failure domain is
published per libvirt host (the host is the failure boundary), with the
verified pools/networks carried in its attributes.

**`VMImage`** — for sources where `providerClass == "libvirt"`:

- `kind: BackingFile` — a pre-existing path on the host; verified against the
  pool's volume list, no import needed.
- `kind: Url` — gated on `VMImage.status.rawDiskArtifact.phase == Ready`
  (written solely by `banlieue-imagebuilder`, ADR-0010). Once ready, one
  import Job per target pool, each running the `banlieue` binary:
  1. mount the artifacts PVC **read-only** at `/artifacts`,
  2. `storage_vol_create_xml` a raw volume sized to the artifact,
  3. `storage_vol_upload` streaming the bytes straight from the PVC.

  Progress is reported per pool in `status.perProvider[].zones[]` — the
  `ZoneImageStatus` list added in ADR-0010, reused here with a pool name where
  vSphere uses a failure-domain name.

`status.rawDiskArtifact` is never written by this provider; `perProvider[]` is
never written by `banlieue-imagebuilder`. The SSA field-manager split from
ADR-0010 carries over unchanged.

### Connection and credentials

`Provider.spec.connection.endpoint` takes a `qemu+tls://host/system` URI. The
existing API carries the rest with no schema change:

- `connection.caBundle` → the CA that signed libvirtd's server certificate.
  Already a value-or-source (inline / `configMapRef` / `secretRef`) per
  ADR-0008.
- `connection.credentialsRef` → a Secret holding the **client certificate and
  key** (`tls.crt` / `tls.key`).

`insecureSkipTLSVerify` is honoured for parity with the vSphere provider but,
as there, is a loud opt-out rather than a convenience.

Because the certificate *is* the credential, there is no password or shared
secret anywhere in the system, and nothing analogous to the SSH host-key
problem that broke this project's cluster bootstrap: server identity is
established by the CA, and `scripts/bootstrap-libvirt-tls.sh` bakes every
address the host answers on into the server certificate's SANs so a
connection by IP cannot fail for want of a name.

### PVC access

The artifacts PVC is created by kairos-operator in the imagebuilder's build
namespace, and the import Jobs run in that same namespace (ADR-0010). On a
`ReadWriteOnce` StorageClass — including the `local-path` provisioner the
reference environment uses — the volume is pinned to one node, but RWO permits
*multiple pods on that node* to mount it, so per-pool Jobs still run
concurrently. No `ReadOnlyMany` requirement is introduced.

## Consequences

**Positive**

- **Zero new dependencies.** No native library, no FFI, no abandoned crate.
  Distroless and Chainguard builds, the OpenSSL-free posture, and macOS→Linux
  cross compilation are all untouched by enabling libvirt.
- **No third-party tools image.** The import Job runs the banlieue binary
  itself, so everything in the data path is covered by banlieue's own SBOM,
  VEX, and signing pipeline (ADR-0006) — nothing outside it to pin or patch.
- **Typed, in-process introspection.** No CLI stdout parsing, and the
  `Provider` probe is two ordinary calls rather than a Job, so there is no
  Job churn and no probe-scheduling heuristic to tune.
- **The transport is strictly better than what it replaces**: mutual TLS in
  place of unencrypted TCP with an obsolete MD5 SASL mechanism.
- Bulk data movement still lives where it belongs: a restartable,
  independently observable, resource-limited Job — not a reconcile loop.
- Gives the reference development environment a provider it can actually run,
  making ADR-0010's pipeline demonstrable end to end for the first time.

**Negative / trade-offs**

- **We now own a wire-protocol implementation.** Procedure numbers and struct
  layouts come from libvirt's `remote_protocol.x`; getting one wrong desyncs
  the stream and fails obscurely. Mitigated by implementing a deliberately
  small surface, unit-testing the codec against captured frames, and pinning
  the negotiated protocol version — but this is real, ongoing maintenance that
  a dependency would have carried. It is the primary cost of this decision.
- **libvirt could change the protocol.** Version increments are supposed to
  signal incompatibility, but nothing guarantees procedure-number stability
  across major releases. An integration test against a real libvirtd is
  therefore not optional.
- **TLS must be provisioned before the provider works at all.** There is no
  plaintext fallback by design, so a host still on TCP/SASL needs
  `bootstrap-libvirt-tls.sh` run first. That is a deliberate trade of
  convenience for not shipping an insecure path.
- **Raw volumes consume full allocated size** until sparse-hole skipping
  lands.
- **A new RBAC surface**: the provider needs `jobs`
  create/get/list/watch/delete and `pods`/`pods/log` read in the build
  namespace, plus Secret read for the client certificate.
- **VM lifecycle stays unbuilt.** This increment cannot create a VM. A
  `LibvirtMachine` InfraMachine CRD and its reconciler are a separate, larger
  piece of work — and the decision here (Jobs for I/O) will need revisiting
  for it, since per-VM power operations are latency-sensitive, small, and
  frequent, i.e. the opposite of image import. Recording that explicitly so
  the Job pattern is not cargo-culted into a domain it does not suit.

## Alternatives considered

- **Run `virsh` inside Jobs for everything (this ADR's first draft).** No
  client code to own and no protocol-drift risk. Rejected on review: it makes
  a CLI's stdout a wire format, and it needs a Job per `Provider` probe just
  to list pools — which in turn needs a staleness heuristic to keep Job churn
  bounded. Owning ~750 lines of well-specified protocol is the better trade
  than owning a screen-scraper plus a scheduling heuristic. Its reasoning
  about the *data* path survives: bulk transfer still runs in a Job.
- **Link the `virt` FFI crate in the controller.** The conventional answer.
  Rejected: a C library in a distroless image, an 11-month-stale dependency,
  and — decisively — a control-plane binary streaming gigabytes.
- **Hybrid: `virt` for control-plane calls, Jobs for bulk transfer.** Keeps
  typed introspection while moving heavy I/O out. Rejected because it still
  pulls `libvirt0` into the image and the stale dependency with it, in
  exchange for introspection amounting to two list calls.
- **Depend on `libvirt-rpc` (pure Rust).** Exactly the shape we want, already
  written. Rejected: last published 2018, far outside the project's
  maintenance rule. Its existence does confirm the approach is tractable.
- **Support plaintext TCP + SASL DIGEST-MD5**, matching how the reference host
  was already configured. Rejected: it would mean implementing an obsolete
  (RFC 6331) MD5 challenge-response *and* pulling in an MD5 dependency,
  purely to speak an unencrypted protocol. More code for a worse security
  posture; provisioning TLS is less work than that.
- **Convert to qcow2 with `qemu-img` in the import Job.** Smaller volumes.
  Rejected for the first increment because it reintroduces the third-party
  tools image the raw path eliminates; sparse streaming inside our own crate
  addresses most of the size concern without that cost.
- **A long-lived helper DaemonSet/sidecar holding a libvirt connection.**
  Rejected as premature: an always-on component to deploy, secure, and
  version — and with in-process introspection there is no longer a problem
  for it to solve.
- **Skip introspection; trust `spec.capabilities` verbatim.** Simplest, and
  defensible under "explicit over implicit". Rejected because a `Provider`
  reporting `Ready` without ever having reached the host is actively
  misleading — a typo'd pool name would surface only later, as a failed
  import.
