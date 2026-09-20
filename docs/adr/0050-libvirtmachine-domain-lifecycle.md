# 0050 — `LibvirtMachine`: the InfraMachine contract on libvirt

- **Status:** Accepted
- **Date:** 2026-09-19

## Context

Roadmap [70](../../.github/community/70-ephemeral-vm-pools.md) wants warm
pools of single-use VMs, and the maintainer has **no vSphere access at
present**, so libvirt is the only backend the work can be tested against. A
`VirtualMachinePool` creates `VirtualMachine`s and nothing else (roadmap 70,
B1); a `VirtualMachine` is only realised when some provider turns it into an
infrastructure CR. On libvirt today, nothing does.

State of the libvirt provider at `8360e19`:

- `crates/banlieue-provider-libvirt/src/reconciler/` contains `provider.rs`
  (capability validation) and `vmimage.rs` (artifact import) only. There is
  **no machine reconciler**.
- `crates/banlieue-api/src/infrastructure/` contains `vsphere_machine.rs` and
  `vsphere_cluster.rs` only. There is **no `LibvirtMachine` CRD**.
- `crates/banlieue-libvirt/src/procs.rs` implements `AUTH_LIST`,
  `CONNECT_OPEN`, `CONNECT_LIST_ALL_STORAGE_POOLS`,
  `CONNECT_LIST_ALL_NETWORKS`, `STORAGE_POOL_LIST_ALL_VOLUMES`,
  `STORAGE_VOL_CREATE_XML` and `STORAGE_VOL_UPLOAD`. There are **no domain
  procedures at all** — nothing defines, starts, stops, undefines or
  inspects a domain.
- `crates/banlieue-controller/src/reconciler/infra.rs` builds
  `VSphereMachine` and says so in its own module doc ("only `vsphere` is
  implemented"); `virtualmachine.rs` types its infra `Api<VSphereMachine>`
  directly.

So the pool layer is not what is missing. **A pool whose members cannot be
realised on the backend tests nothing**, and that makes this ADR a
prerequisite of roadmap 70's live test rather than the phase-D amendment the
roadmap originally described.

### Roadmap 13's client choice is already superseded

`.github/community/13-phase-1d-libvirt-provider.md` specifies the `virt`
crate (libvirt FFI bindings) and a `debian:bookworm-slim` base image carrying
`libvirt0`. The repo did not go that way: `crates/banlieue-libvirt` is a
pure-Rust implementation of libvirt's **native RPC protocol** — XDR codec
(`xdr.rs`), message framing (`rpc.rs`), session and stream handling
(`transport.rs`), and the procedure layer (`procs.rs`), each with
transcription notes back to `remote_protocol.x` and traces taken from a real
`virsh`. It already carries the hard parts: optional-pointer XDR encoding,
bounded array counts, the `vol-upload` stream protocol, and TLS/auth
negotiation.

Adopting `virt` now would mean a **second** libvirt client in the same
binary, `unsafe` FFI boundaries, a distroless→`bookworm-slim` base image
change, and a `libvirt0` system dependency in the supply chain — to reach
procedures that are ten lines of XDR each in the client that already exists.

### Procedure numbers and wire layouts

Transcribed from `src/remote/remote_protocol.x` (libvirt `master`, fetched
2026-09-18) and `include/libvirt/libvirt-domain.h`, not inferred:

| Procedure | # |
|---|---|
| `REMOTE_PROC_DOMAIN_LOOKUP_BY_NAME` | 23 |
| `REMOTE_PROC_DOMAIN_DESTROY` | 12 |
| `REMOTE_PROC_DOMAIN_SHUTDOWN` | 33 |
| `REMOTE_PROC_DOMAIN_CREATE_WITH_FLAGS` | 196 |
| `REMOTE_PROC_DOMAIN_GET_STATE` | 212 |
| `REMOTE_PROC_DOMAIN_UNDEFINE_FLAGS` | 231 |
| `REMOTE_PROC_CONNECT_LIST_ALL_DOMAINS` | 273 |
| `REMOTE_PROC_DOMAIN_DEFINE_XML_FLAGS` | 350 |
| `REMOTE_PROC_DOMAIN_INTERFACE_ADDRESSES` | 353 |
| `REMOTE_PROC_STORAGE_VOL_DELETE` | 94 |
| `REMOTE_PROC_STORAGE_VOL_LOOKUP_BY_NAME` | 95 |

`remote_nonnull_domain` is `{ string name; uuid[16]; int id; }` — the domain
handle every domain call takes, and the thing `DOMAIN_LOOKUP_BY_NAME` and
`DOMAIN_DEFINE_XML_FLAGS` return. `REMOTE_DOMAIN_INTERFACE_MAX` and
`REMOTE_DOMAIN_IP_ADDR_MAX` are both 2048; `REMOTE_DOMAIN_LIST_MAX` is 16384.

Relevant flag values from `libvirt-domain.h`: `VIR_DOMAIN_UNDEFINE_MANAGED_SAVE
= 1<<0`, `VIR_DOMAIN_UNDEFINE_NVRAM = 1<<2`, `VIR_DOMAIN_UNDEFINE_TPM =
1<<5`; interface-address sources `LEASE = 0`, `AGENT = 1`, `ARP = 2`; domain
states `RUNNING = 1`, `PAUSED = 3`, `SHUTDOWN = 4`, `SHUTOFF = 5`,
`CRASHED = 6`, `PMSUSPENDED = 7`.

The undefine flags are not a detail. `.wolf/cerebrum.md` already records a
live incident (2026-07-29) where `virsh undefine` silently failed on UEFI
domains for want of `--nvram`, leaving every VM defined while teardown
reported success.

## Decision

1. **New CRD `LibvirtMachine`** in
   `crates/banlieue-api/src/infrastructure/libvirt_machine.rs`, satisfying
   the CAPI v1beta2 InfraMachine contract exactly as `VSphereMachine` does —
   `providerID`, `failureDomain`, `status.initialization.provisioned`,
   `status.addresses`, `status.conditions` — so the contract label is emitted
   by `crdgen` the same way (ADR-0005). Non-negotiable #2 holds for libvirt
   identically to vSphere.

   `providerID` format: `libvirt://<provider-name>/<domain-uuid>`. The
   provider name rather than a hostname, because the connection URI is a
   deployment detail that can change without the VM changing, and because a
   hostname in a provider ID is the kind of value
   `rules/no-real-infrastructure.md` exists to keep out of tracked YAML.

2. **`LibvirtMachineTemplate` alongside it**, matching
   `VSphereMachineTemplate` exactly: a `template.spec` wrapper carrying a
   `LibvirtMachineSpec`, per CAPI's InfraMachineTemplate shape. Nothing in
   banlieue consumes an InfraMachineTemplate today — it exists so a CAPI
   `MachineSet`/`MachineDeployment` can stamp out machines, which is the
   point of Non-Negotiable #2. Shipping the machine kind without its template
   would make libvirt the one backend that is *not* usable as a CAPI infra
   provider, which is the opposite of what this ADR is for.

3. **Domain procedures go into `crates/banlieue-libvirt` over the native
   protocol. Roadmap 13's `virt` FFI choice is superseded** and that document
   is amended to say so. Add to `rpc.rs`/`procs.rs`, following the existing
   transcription-comment convention, with pure `encode_*`/`decode_*` halves
   unit-tested against known-good byte sequences and no connection:
   `domain_lookup_by_name`, `domain_define_xml_flags`,
   `domain_create_with_flags`, `domain_shutdown`, `domain_destroy`,
   `domain_undefine_flags`, `domain_get_state`, `domain_interface_addresses`,
   `list_all_domains`, `storage_vol_lookup_by_name`, `storage_vol_delete`.

4. **A redefine names the existing domain's UUID.** `DOMAIN_DEFINE_XML` is
   *not* an unconditional upsert, contrary to what this ADR originally
   claimed: libvirt identifies a domain by UUID, so a document with no
   `<uuid>` gets a freshly generated one and the define is then refused with
   *"already exists with uuid …"*. The reconciler therefore looks the domain
   up before rendering and carries its UUID into the document.

   Found by the live test on 2026-09-19, not by review: the first converge
   succeeded and every subsequent one failed, so a VM would come up and then
   never reconcile again. The offline fake had modelled define as an
   overwrite, which is why nothing caught it earlier — it has been corrected
   to refuse the same way libvirtd does.

5. **Undefine always passes `MANAGED_SAVE | NVRAM | TPM`** (`1<<0 | 1<<2 |
   1<<5` = `0x25`). Not conditional on whether the domain is EFI or has a
   vTPM: the flags are harmless when there is nothing to remove, and the
   failure mode of omitting them is a silent leak of NVRAM varstores and
   swtpm state per deleted VM. A teardown step is never `|| true`-ed and its
   effect is verified by re-listing, per the recorded incident.

6. **Domain XML is built by a dedicated `xml` module with mandatory
   escaping**, tested against golden files. Every value that reaches XML
   (domain name, volume paths, network/bridge names, MAC) is user- or
   admin-influenced, so `format!` of a raw string into markup is prohibited;
   the escaping helper is the only way values enter a template.

7. **Two disk shapes, chosen by install mode**, mirroring what ADR-0040
   forces on vSphere:
   - `Immediate` — overlay a qcow2 on the imported backing file.
   - `Deferred` — create an **empty** volume, attach the image ISO as a
     CD-ROM, boot, let the guest install itself. No template clone at all.
     This is the shape roadmap 70's pool members need, and on libvirt it is
     *simpler* than the `Immediate` path rather than harder.

8. **Address discovery is source-ordered, not source-fixed**:
   `AGENT` (1) first, falling back to `LEASE` (0), falling back to `ARP` (2).
   Agent is authoritative when `qemu-guest-agent` is in the image; leases
   work for libvirt-managed networks; ARP is best-effort on bridges. The
   chosen source is recorded on status so a wrong address is diagnosable.

9. **Controller dispatch by provider class.**
   `banlieue-controller`'s `infra.rs` gains a builder for `LibvirtMachine`
   beside `build_vsphere_machine`, selected by the resolved `Provider`'s
   class; `virtualmachine.rs`'s infra handling stops being typed
   `Api<VSphereMachine>` and dispatches on that class for apply, delete and
   cascade-wait. `status_mirror.rs` needs only a new `impl InfraMachineRead
   for LibvirtMachine` — that trait already exists and was written for
   exactly this.

10. **vTPM is out of scope here** and is ADR-0051. This ADR gets a VM booting
   on libvirt; sealing its disk to a per-VM swtpm device is a separate
   decision with its own state-lifecycle and never-clone consequences.

11. **Live migration stays out.** `migrationPolicy` other than `Never`
    yields a `PlacementValid=False` condition and a recreate, matching the
    existing placeholder in `migration.rs` (ADR-0036).

## Consequences

- Roadmap 13 is amended, not replaced: its NoCloud cloud-init ISO, IPAM,
  SSH-key Secret and failure-domain sections stand. Its "Libvirt client
  choice" section and the `virt` dependency are struck, and its module layout
  loses `client/` in favour of procedures in `banlieue-libvirt`.
- The provider image stays distroless. No `libvirt0`, no `qemu-utils`, no
  `genisoimage` system dependency is introduced by this ADR — the NoCloud ISO
  builder remains a pure-Rust concern to be decided when it is written.
- No `unsafe` enters the provider. The FFI-safety and connection-lifetime
  gotchas roadmap 13 lists for the `virt` crate stop applying.
- Every domain procedure is unit-testable without libvirtd, because the
  encode/decode halves are plain functions over bytes — the same property
  that made the storage procedures testable.
- `banlieue-controller` gains its first real multi-provider code path. That
  is load-bearing beyond libvirt: Proxmox (roadmap 12) becomes a third
  builder rather than a second special case.
- Until ADR-0051 lands, a `VMClass` with `tpmEnabled: true` cannot schedule
  onto a libvirt Provider, because no libvirt failure domain advertises
  `FEATURE_VTPM` (ADR-0039 Decision 2). This is the correct behavior in the
  interim: refusing to schedule beats attaching nothing and encrypting
  nothing, which is exactly the silent failure ADR-0048 exists to close.
- A wrong procedure number is a silent wire bug, so the numbers above are
  transcribed with their source and version, and the first live call against
  a real libvirtd is the acceptance test for each one.
