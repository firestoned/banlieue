<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Guest Configuration Hygiene

**Rule: guest configuration carries no secrets.** No passwords, API tokens,
private keys, cloud credentials, join tokens or long-lived SSH keys go into
`spec.userData`, cloud-config, guestinfo or a NoCloud seed.

The only tolerated exception is a **single-use bootstrap value that is inert
without the VM's vTPM proof**, for example a one-time enrolment nonce that a
broker redeems only together with an attestation from that VM's own vTPM
(ADR-0045, ADR-0049). A value that grants anything on its own is a secret.

This page says what banlieue puts into a guest and who else can read it on
the way. It is the reason for the rule.

## Where user data comes from

`VirtualMachine.spec.userData` is never inline. It names exactly one of a
Secret or a ConfigMap, plus a key (default `user-data`)
(`crates/banlieue-api/src/banlieue/virtualmachine.rs`, `UserDataSpec`). The
`banlieue-virtualmachine-userdata-authorization` admission policy requires
the VM's creator to be able to `get` that object (ADR-0042).

The controller reads it (`crates/banlieue-controller/src/reconciler/virtualmachine.rs`,
`resolve_rendered_user_data`) and substitutes `${VM_NAME}`, `${FQDN}`,
`${IP}`, `${PREFIX}`, `${GATEWAY}`, `${DNS}` and `${DOMAIN}`
(`crates/banlieue-provider-sdk/src/guestdata.rs`, `render_placeholders`).
Nothing scans the content for secret material; the only check is that it
is valid UTF-8.

## Where it goes, and who can read it there

Referencing a Secret does **not** keep the content secret. The rendered
text is copied in plaintext at each step:

| Copy | Location | Readable by |
| --- | --- | --- |
| Infra machine spec | `VSphereMachine.spec.userData`, `LibvirtMachine.spec.userData`, `CloudHypervisorMachine.spec.userData` (set in `crates/banlieue-controller/src/reconciler/infra.rs`) | Anyone with `get` on that infra kind in the namespace, and etcd backups. See threat model asset A-2 |
| vSphere guestinfo | `guestinfo.userdata` (base64) in the VM's `extraConfig`, applied at clone | Anyone with read on the VM in vCenter, and the guest itself for its lifetime |
| libvirt NoCloud seed | A `CIDATA` ISO volume in the storage pool, attached as a CD-ROM (ADR-0054) | Anyone with access to the storage pool, and the guest |
| Cloud Hypervisor seed | `<machine dir>/seed.iso`, mode `0440`, attached read-only | The provider's host user, host root, and the guest |

Everything in user data is also readable by any process in the guest that
can read the config drive or query guestinfo. For a sandbox VM, that means
the agent.

## What banlieue itself writes into a guest

Besides the user's own data, banlieue adds only non-secret identity and
network facts:

| Provider | Keys or files |
| --- | --- |
| vSphere | `guestinfo.network.hostname`; `guestinfo.network.ip`, `.prefix`, `.gateway`, `.dns`, `.domain` (static addressing only); `guestinfo.metadata` (`instance-id`, `local-hostname`); `guestinfo.userdata` and `guestinfo.userdata.encoding` when user data is set; `ethernetN.pciSlotNumber` (`crates/banlieue-provider-vsphere/src/reconciler/vspheremachine.rs`, `build_guestinfo`) |
| libvirt | NoCloud `meta-data` (`instance-id` = domain UUID, `local-hostname`) and `user-data` if set (`crates/banlieue-provider-sdk/src/cloudinit/mod.rs`) |
| Cloud Hypervisor | The same NoCloud seed, built only when user data is set |

The `GuestReady` signal (ADR-0043) runs the other way: the guest writes a
marker (`guestinfo.banlieue.phase` on vSphere, `/run/banlieue/phase` on
libvirt, a vsock report on Cloud Hypervisor) and the provider only reads
it. On libvirt it is read with `guest-file-read` only, never `guest-exec`.

## What to do instead

- Put credentials the workload needs behind an identity the VM proves at
  runtime (vTPM attestation, then a short-lived token). For sandboxes this
  is [mediatore](../guides/sandbox-identity-mediatore.md)'s job.
- Keep user data to configuration: packages, users without passwords, the
  `GuestReady` marker writer, and the address of the broker.
- Do not add SSH authorized keys to sandbox user data. Sandboxes accept no
  inbound connections (ADR-0082).
- If you must use a bootstrap value, make it single-use, short-lived and
  bound to the VM's vTPM, as above.

!!! note "Not enforced yet"
    banlieue does not currently lint user data for secret material. This
    rule is a deployment obligation until that lands. The alignment report
    for ADR-0081 and ADR-0082 tracks it as a candidate change.
