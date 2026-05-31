<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0004 — Single `banlieue` binary with subcommand dispatch

- **Status:** Accepted
- **Date:** 2026-05-30
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0003 (provider deployment topology); D-003 (CRD-only bus);
  workspace layout in `.claude/CLAUDE.md`. Independent of ADR-0001/0002 (CAPI
  contract) — this is a packaging/entrypoint decision, not a contract change.

## Context

Today the workspace ships one executable per role:

- `banlieue-controller` — `crates/banlieue-controller/src/main.rs`
- `banlieue-provider-vsphere` — `crates/banlieue-provider-vsphere/src/main.rs`

Each `main.rs` re-implements the same bootstrap: CLI parsing, `tracing`
init, a minimal health server, SIGTERM/Ctrl-C shutdown, leader-election
wiring, then the role-specific `Controller`s. The health-server,
shutdown-signal, and leader-config code is duplicated verbatim across both
binaries; `init_tracing` differs only by one filter directive.

Every new provider (`proxmox`, `libvirt`, …) would add another binary,
another `main.rs`, another image to build/tag/scan/deploy, and another copy
of the bootstrap boilerplate. Operationally this multiplies the
supply-chain surface (one image per role) and the Makefile/CI/Dockerfile
matrix grows linearly with provider count.

We want **one place** to install and run banlieue, while keeping each role
genuinely independent — its own crate, its own `Cargo.toml`, its own
dependency graph (the vSphere provider pulls the heavy `vim_rs`; the
controller must not).

Options considered:

1. **Status quo — one binary per role.** Maximum isolation of build deps;
   maximum duplication and image sprawl. Rejected: every provider adds a
   full binary + image + bootstrap copy.
2. **One monolithic crate** holding controller and all providers in a
   single `Cargo.toml`. Single binary, but the roles are no longer
   independent — the controller build is forced to compile `vim_rs`, and
   crate boundaries that today enforce the CRD-only seam collapse. Rejected:
   violates the "each role independent" requirement.
3. **Thin aggregator binary over independent library crates.** Each role
   crate becomes a library exposing a `clap` args struct + an async
   `run(args)` entry point. A new `banlieue` crate is the *only* binary; it
   dispatches `banlieue controller` / `banlieue provider <name>` to the
   matching library. Providers are gated behind per-provider Cargo features
   so a slim build can drop a backend's deps entirely.

## Decision

Adopt **option 3**. Concretely:

- New crate `crates/banlieue` produces the single `banlieue` executable. It
  owns only CLI dispatch — no reconcile logic.
- The CLI shape is:
  - `banlieue controller [flags]`
  - `banlieue provider <name> [flags]` (e.g. `banlieue provider vsphere`)
  `provider` is a subcommand group; each backend is a nested subcommand.
- `banlieue-controller` and `banlieue-provider-vsphere` become **library
  crates** (no `[[bin]]`, no `main.rs`). Each exposes a public `Cli`
  (`clap::Args`) and `pub async fn run(cli: Cli) -> anyhow::Result<()>`
  that owns that role's full lifecycle.
- Providers are gated by **per-provider Cargo features** on the `banlieue`
  crate (`vsphere`, future `proxmox`/`libvirt`), with `default = ["vsphere"]`
  (all available backends on by default). `--no-default-features --features
  vsphere` yields a single-backend build; the disabled provider's crate and
  its transitive deps (e.g. `vim_rs`) are not compiled or linked.
- Shared bootstrap (health server, shutdown signal, `tracing` init) moves
  into `banlieue-provider-sdk` as a `bootstrap` module so every role and
  every future provider reuses one implementation. This is where the
  duplication is eliminated; the SDK already exists for exactly this kind of
  shared runtime helper.
- One container image (`banlieue`) ships the one binary. Deployments select
  the role via container `args` (`["controller"]`,
  `["provider","vsphere"]`), not via distinct images. The Dockerfile's
  `ENTRYPOINT ["/app"]` is unchanged; `args` flow through as subcommands.

Deployment topology (ADR-0003) is unchanged and orthogonal: the same image
runs as N differently-argued Deployments. `Shared` vs `PerInstance` still
governs how many Deployments exist, not how many binaries.

## Consequences

**Positive**

- One artifact to build, tag, sign, scan, publish, and install. Adding a
  provider adds a feature + a nested subcommand, not a new binary/image.
- Bootstrap boilerplate exists once (in the SDK), not once per role.
- Crate independence is preserved: each role keeps its own `Cargo.toml` and
  dep graph; the CRD-only seam between controller and providers is intact
  (they still never call each other — only the K8s API).
- Slim builds remain possible via features, so an operator who only runs
  vSphere need not ship `proxmox`/`libvirt` code.

**Negative / trade-offs**

- The default `banlieue` build links every enabled provider, so the default
  image carries every backend's deps (incl. `vim_rs`). Mitigated by the
  feature flags for those who need a minimal image.
- A single image means a CVE in any provider's dep surface is "in" the one
  image even for operators not using that provider (unless they build with
  features). Accepted: the convenience of one artifact outweighs it, and the
  feature path exists for hardened/minimal deployments.
- Deploy manifests, Makefile (`WORKSPACE_BINARIES`, `BINARY`), and image
  names change in one pass (`banlieue-controller` → `banlieue` + `args`).

**Follow-ups**

- When `proxmox`/`libvirt` land, add a feature + nested subcommand each;
  no new binary.
- Per-provider image variants (feature-sliced) can be added later if a
  minimal-surface requirement appears, without changing this decision.
