<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0074 — `banlieue-proxmox`: a first-party Proxmox VE REST client, API tokens only

- **Status:** Accepted
- **Date:** 2026-09-28
- **Deciders:** Erick Bourgeois
- **Amended:** 2026-09-30 (Decisions 4 and 7, from the first live lifecycle run:
  seed deletion needs `Datastore.Allocate`, granted by a second role on a
  dedicated seed storage only; `resize` is a task on PVE 9)
- **Related:** Closes O-001 and completes D-006 in
  [roadmap 01](../../.github/community/01-decisions.md);
  [roadmap 06](../../.github/community/06-phase-1c-proxmox-provider.md);
  [ADR-0011](0011-libvirt-provider-own-client.md) (the libvirt precedent),
  [ADR-0061](0061-banlieue-cloud-hypervisor-vmm-client.md) (the Cloud
  Hypervisor precedent), [ADR-0008](0008-byoc-vsphere-http-client.md) /
  [ADR-0009](0009-vim-rs-0.5-rustls-ring-retire-vendoring.md) (the TLS posture reused
  here); [ADR-0075](0075-proxmoxmachine-inframachine-contract.md) (the
  machine this client serves).

## Context

Roadmap 06 leaves one decision open before any Proxmox code: which client
talks to Proxmox VE (O-001). The candidates on crates.io on 2026-09-28:

| Crate | Notes |
|---|---|
| `proxmox-client` 0.9 | Community; broad generated surface; brings its own HTTP stack and TLS choices |
| `proxmox-api` 0.2 | Community bindings; pre-1.0, low activity |
| `leeca_proxmox` 0.3 | Community SDK; pre-1.0 |
| `proxmox` 0.0.0 | Name placeholder |

Proxmox's own Rust crates (`proxmox-*` in `git.proxmox.com`) target PBS and
the Proxmox backend itself, are not published for PVE client use, and are
not on crates.io at usable versions.

What the provider actually needs is small and stable — about twenty
endpoints of a JSON-over-HTTPS API that is versioned by path (`/api2/json`)
and documented at <https://pve.proxmox.com/pve-docs/api-viewer/>:

| Area | Endpoints |
|---|---|
| Identity | `GET /version` |
| Inventory | `GET /nodes`, `GET /cluster/resources?type=vm`, `GET /cluster/nextid`, `GET /nodes/{n}/storage`, `GET /nodes/{n}/network`, `GET /nodes/{n}/storage/{s}/content` |
| Lifecycle | `POST /nodes/{n}/qemu/{id}/clone`, `GET`/`PUT /nodes/{n}/qemu/{id}/config`, `PUT …/resize`, `POST …/status/{start,stop,shutdown}`, `GET …/status/current`, `DELETE /nodes/{n}/qemu/{id}` |
| Media | `POST /nodes/{n}/storage/{s}/upload`, `DELETE /nodes/{n}/storage/{s}/content/{volid}` |
| Tasks | `GET /nodes/{n}/tasks/{upid}/status` |
| Guest | `GET …/agent/network-get-interfaces` |

Three properties of this API shape the client more than its size does:

- **Errors live in the HTTP reason phrase.** Proxmox answers
  `401 Authentication failed!` or `500 Configuration file … does not exist`
  with an empty body. A client that reports only the status code turns every
  failure into "500".
- **Mutations are asynchronous.** A clone, start, stop, delete or upload
  returns a UPID (`UPID:<node>:<pid>:<pstart>:<starttime>:<type>:<id>:<user>:`)
  and a 200 that means *queued*, not *done* (roadmap 06, Gotchas).
- **Two auth schemes.** A ticket (`POST /access/ticket` with a password,
  a two-hour lifetime, and a CSRF header on every write) or an API token
  (`Authorization: PVEAPIToken=<user>@<realm>!<id>=<uuid>`, stateless).

## Decision

1. **A first-party client crate, `crates/banlieue-proxmox`**, beside
   `banlieue-libvirt` and `banlieue-cloud-hypervisor`, and on the same
   terms: protocol only, no `kube`, no banlieue API types. Every dependency
   is one the workspace already locks — `reqwest` and `rustls` at their
   workspace pins, `form_urlencoded` (already in `Cargo.lock` under `url`)
   for request bodies — so the crate compiles **no new third-party code**.
   reqwest's `form`, `query` and `multipart` features are deliberately *not*
   enabled: each pulls a crate the lock does not have
   (`serde_urlencoded`, `mime_guess`), and the one multipart body the client
   sends (an ISO upload) is ten lines to write by hand.

2. **The client is a trait, `ProxmoxApi`, with one HTTP implementation.**
   Reconcilers take `Arc<dyn ProxmoxApi>`, so the provider's unit tests
   drive them against an in-memory fake, and the fake must refuse what the
   real API refuses (`rules/testing.md`, rule 1): cloning onto a VMID that
   exists fails, deleting a running VM fails, a task that fails reports its
   exit status.

3. **API tokens only.** The provider authenticates with
   `PVEAPIToken=<tokenid>=<secret>`, read from the Provider's credentials
   Secret keys `username` (the full `user@realm!tokenid`) and `tokenValue`
   — the keys `ProviderConnection`'s documentation already names. The
   ticket/password path is **not implemented**: it would need a renewal
   loop, CSRF handling, and a reusable password in a Secret, where a token
   is stateless, revocable on its own, and can be **privilege-separated**
   (it holds only the ACLs granted to the token, never its user's). The
   `username`/`password` alternative in that documentation is removed.

   The token id is validated at connect (`user@realm!id`, each part
   non-empty) before any request, because a mis-pasted token otherwise
   surfaces as a bare `401`.

4. **Least privilege is part of the contract, not an install note.** The
   provider needs exactly one role, `BanlieueProvider`:
   `VM.{Allocate,Audit,Clone,PowerMgmt}`,
   `VM.Config.{CDROM,CPU,Cloudinit,Disk,HWType,Memory,Network,Options}`,
   `VM.GuestAgent.Audit`, `Datastore.{AllocateSpace,AllocateTemplate,Audit}`,
   `SDN.{Audit,Use}`, `Sys.Audit`, granted on `/vms`, the target storages,
   the SDN zone and the node(s) only. It never needs `VM.Console`,
   `VM.GuestAgent.{FileRead,FileWrite,Unrestricted}` (guest exec),
   `VM.Migrate`, `VM.Snapshot*`, `VM.Backup`, `Sys.Modify` or
   `Permissions.Modify`. `scripts/bootstrap-proxmox-host.sh` creates exactly
   this, and the provider guide documents it.

   *Amended 2026-09-30.* Deleting a seed ISO needs **`Datastore.Allocate`**:
   Proxmox gates removal of every non-backup volume on it, which the first
   live lifecycle run found (`Permission check failed (/storage/local,
   Datastore.Allocate)`), so without it no seed could ever be cleaned up. On a
   shared storage that privilege also deletes backups, templates and other
   ISOs, and edits the storage definition. So it lives in a **second role,
   `BanlieueSeed`** (`Datastore.{Allocate,AllocateTemplate,Audit}`), granted
   **only on a dedicated `dir` storage holding nothing but seeds**
   (`banlieue-seed`, content `iso`, created by the script's `seed` step). The
   `BanlieueProvider` role no longer needs any grant on the ISO storage, and
   the token holds nothing on `local`.

5. **TLS is verified by default, against a supplied CA.** Proxmox serves a
   certificate issued by the node's own `PVE Cluster Manager CA`. The
   Provider's `connection.caBundle` carries that CA
   (`/etc/pve/pve-root-ca.pem`); with no bundle, the system roots apply (a
   node behind a publicly-trusted certificate). The client builds its own
   `reqwest` client on the process-wide `ring` provider (ADR-0009), exactly
   as the vSphere BYOC client does (ADR-0008). `insecureSkipTLSVerify`
   remains available and remains gated by the admission policy that already
   covers the `proxmox` class (`deploy/admission/provider-connection.yaml`).

6. **Errors carry the reason phrase.** Non-2xx responses become
   `Error::Api { status, message }` with Proxmox's message taken from the
   reason phrase (reqwest's `error_for_status_ref` keeps hyper's
   `ReasonPhrase`), falling back to the body's `errors` map for parameter
   validation failures.

7. **UPIDs are awaited, bounded.** Every asynchronous call returns a typed
   `Upid`; `wait_task` polls `…/tasks/{upid}/status` until `stopped` and
   treats any `exitstatus` other than `OK` as an error carrying it.
   *Amended 2026-09-30:* `PUT …/resize` is one of these on PVE 9 (it returns
   a UPID; older releases returned `null`), so the client returns
   `Option<Upid>` for it and the provider waits before starting the guest. Polling
   is bounded by a caller-supplied timeout and a fixed interval, and a
   timeout is an error rather than a silent return — a clone that never
   finishes must not look like one that did. This is polling a remote task
   the reconciler itself started, not polling Kubernetes; the controllers
   remain watch-driven.

8. **Every request has a timeout** (connect and total), so a wedged
   `pveproxy` cannot stall a reconcile loop indefinitely.

## Consequences

- One more protocol crate to maintain. The surface is small, versioned by
  path, and exercised by a live test against a real node
  (`make proxmox-live-test`), which is where a drifted field would show up.
- No ticket auth means no password-only Proxmox deployments. That is the
  intent; a token is strictly better for an unattended controller.
- The privilege list in Decision 4 is now a contract: an endpoint added to
  the client that needs a new privilege must update the role, the bootstrap
  script, the guide and the threat model together.
- Snippet upload is not in the API at all (`upload` accepts only `iso`,
  `vztmpl` and `import` content on PVE 9.2), which is why ADR-0075 delivers
  cloud-init as a NoCloud ISO rather than roadmap 06's `cicustom` snippet.

## Alternatives considered

- **`proxmox-client`.** The broadest community crate, but it chooses its
  own TLS and HTTP stack, would add a dependency tree banlieue does not
  otherwise carry, and covers hundreds of endpoints to reach twenty. The
  same reasoning ADR-0011 applied to the `virt` crate.
- **Ticket auth as a fallback.** Rejected in Decision 3: stateful, needs a
  password Secret, and gives the controller its user's full privileges.
- **Shelling out to `pvesh` over SSH.** Needs root SSH on the hypervisor and
  makes a CLI's stdout a wire format — the path ADR-0011 already rejected.
