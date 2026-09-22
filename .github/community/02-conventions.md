# banlieue — Coding Conventions

## Code style

- `rustfmt` defaults. Run `cargo fmt --all` before every commit.
- `clippy` clean with `-D warnings`. Allow only with explicit
  `#[allow(clippy::...)]` and a comment justifying it.
- Module organization: one CRD-ish struct per file, common types
  hoisted to `common.rs`.

## Errors

- Library crates (`banlieue-api`, `banlieue-provider-sdk`,
  `banlieue-provider-*`): typed errors via `thiserror`.
- Binaries: typed errors propagate up; `main` may use `eyre::Result`
  for pretty panics.
- **Never `unwrap()` in reconciler code paths.** A panic in a
  reconciler kills the controller.

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("kube api: {0}")]
    Kube(#[from] kube::Error),

    #[error("vsphere: {0}")]
    Vsphere(#[from] vim_rs::Error),

    #[error("provider {name} has no failure domain matching constraints")]
    NoCandidateFailureDomain { name: String },

    #[error("image {image} not ready on provider {provider}")]
    ImageNotReady { image: String, provider: String },
}

pub type Result<T> = std::result::Result<T, Error>;
```

## Logging

- `tracing` everywhere.
- Each reconcile opens a span:

```rust
let span = tracing::info_span!(
    "reconcile",
    kind = "VirtualMachine",
    namespace = %ns,
    name = %name,
    generation = obj.metadata.generation.unwrap_or(0),
);
let _enter = span.enter();
```

- Log level guidance:
  - `error!` — operator-visible problem requiring intervention
  - `warn!` — recoverable problem, will retry
  - `info!` — state transitions worth recording
  - `debug!` — verbose, on by default in dev only
  - `trace!` — per-step detail

## Reconciliation pattern

Every reconciler follows this skeleton:

```rust
async fn reconcile(obj: Arc<Foo>, ctx: Arc<Context>) -> Result<Action> {
    let ns = obj.namespace().ok_or(Error::MissingNamespace)?;

    // 1. Handle deletion via finalizer
    if obj.metadata.deletion_timestamp.is_some() {
        return finalize(&obj, &ctx).await;
    }
    ensure_finalizer(&obj, &ctx).await?;

    // 2. Reconcile spec → desired state
    let desired = compute_desired(&obj, &ctx).await?;

    // 3. Apply (server-side) owned objects
    apply_owned(&desired, &ctx).await?;

    // 4. Observe (read back state from backend / k8s)
    let observed = observe(&obj, &ctx).await?;

    // 5. Patch status (never replace)
    patch_status(&obj, &observed, &ctx).await?;

    Ok(Action::requeue(Duration::from_secs(30)))
}
```

Key rules:

- **Idempotent.** Running twice in a row must produce the same result.
- **Patch, don't replace.** Use `Patch::Merge` or `Patch::Apply`.
- **Finalizers for cleanup** of external resources (VMs, images).
- **Server-side apply** for owned objects you create
  (infra CRs, IPAddressClaims, Secrets).
- Returned `Action` always specifies a requeue interval; default 30 s,
  longer for stable terminal states.

## Status updates

- Always set `status.observedGeneration` to `metadata.generation`.
- Conditions use `metav1.Condition` and the stable types/reasons in
  `banlieue_api::common::condition_types` /
  `banlieue_api::common::condition_reasons`.
- Patch `status` as a subresource:

```rust
let api: Api<VirtualMachine> = Api::namespaced(client.clone(), &ns);
api.patch_status(
    &name,
    &PatchParams::apply("banlieue.io/controller").force(),
    &Patch::Apply(status_patch),
).await?;
```

## Owner references

- Anything created by a controller for a parent CR must have an
  `ownerReference` to that parent with `controller: true,
  blockOwnerDeletion: true`.
- This makes `kubectl delete vm db-prod-01` garbage-collect the
  `VSphereMachine`, `IPAddressClaim`s, and `Secret`s automatically.

## CRD authoring

- Use `kube::CustomResource` derive.
- Always include printer columns: at minimum `Ready` and `Age`.
- Always `derive = "PartialEq"` on the wrapper to enable hash-based
  caching.
- Subresources: `status` for everything with status; `scale` only
  where genuinely scalable (e.g. `VirtualMachineSet` later, not now).

## Naming

| Construct | Convention |
|---|---|
| CRD kind | `PascalCase` matching the Rust type that wraps the spec |
| CRD plural | lowercase concatenation: `virtualmachines`, not `virtual-machines` |
| API group | `banlieue.io`, `infrastructure.banlieue.io` |
| Condition type | `PascalCase`: `Ready`, `InfrastructureReady` |
| Condition reason | `PascalCase`: `Cloning`, `PoweredOn` |
| Label / annotation | `banlieue.io/<thing>` |

## Testing

> Binding statement of these rules: `rules/testing.md` + the `tdd-workflow`
> skill. **Tests are written first** — failing test, then the minimum
> implementation, then refactor.

- **Unit tests live in a separate `_tests.rs` file — never embedded in the
  source file.** `src/foo.rs` gets `#[cfg(test)] mod foo_tests;` at the
  bottom; `src/foo_tests.rs` contains
  `#[cfg(test)] mod tests { use super::super::*; … }`.
  (The earlier "next to code in `#[cfg(test)] mod tests`" convention is
  superseded — see D-022.)
- **Integration tests** live in `tests/`; use a fake client or stand up a
  `kind` cluster.
- For reconcilers, prefer **table-driven tests** that exercise:
  - Happy path (create → ready)
  - Scheduling failure (no candidate)
  - Status mirroring (infra goes ready → VM goes ready)
  - Deletion via finalizer
- Mock external clients behind a trait so tests don't need a real
  vCenter / Proxmox / libvirt.

## Admission control (no webhooks)

**There are no admission webhooks in banlieue** — see D-018 and
[ADR-0007](../../docs/adr/0007-admission-policies.md). Do not add one
without an ADR that supersedes that decision.

- Validation is CEL **`ValidatingAdmissionPolicy`**, one file per concern
  under `deploy/admission/`. Keep a rule that needs a newer apiserver
  capability in its own policy file so it can be gated independently.
- **Defaulting is schema-level**: `#[serde(default = "…")]` on the Rust type
  flows into the generated CRD's `default:`, and the apiserver applies it.
  `firmware`, `migrationPolicy`, `desiredPowerState`, `provisioning` and the
  IPAM `source` default this way — there is nothing to mutate at admission.
- Validation enforces immutability, reference consistency and authorization
  (the `authorizer` CEL variable, so a creator can only name a Secret they
  can actually read).

## Container images

- **One image for every role.** There is a single `banlieue` binary with
  subcommand dispatch (`banlieue controller`, `banlieue provider vsphere`)
  per [ADR-0004](../../docs/adr/0004-single-binary-subcommand-dispatch.md);
  the Deployment selects the role through container `args`. There is no
  `banlieue-controller` / `banlieue-provider-*` image.
- The binary is **built in CI** by `firestoned/github-actions`
  (`rust/build-binary`) and copied in — the `Dockerfile` has no Rust builder
  stage, so there is no `rust:<version>` base to keep in sync.
- Runtime base: `gcr.io/distroless/cc-debian13:nonroot`, digest-pinned on a
  literal `FROM` line (Dependabot's Docker parser does not expand `ARG`).
  `Dockerfile.chainguard` is the alternate base.
- `USER nonroot`; `ENTRYPOINT ["/usr/local/bin/banlieue"]`.
- Health check: `/healthz` (liveness) and `/readyz` (readiness) on
  port 8081, provided by `banlieue-provider-sdk::bootstrap` for every role.

## CI (GitHub Actions, target shape)

- `fmt`, `clippy`, `test` on every PR.
- `crdgen` runs and verifies the output matches `deploy/crds/`
  (regenerate locally if it doesn't; CI fails otherwise).
- E2E on `kind`
  ([ADR-0014](../../docs/adr/0014-kind-e2e-operator-contract.md)).
- DCO check via [DCO app](https://github.com/apps/dco).

Two standing rules for workflows (`rules/github-workflows.md`):
**all logic lives in Makefile targets** — a workflow may install tools, set
env, and call `make <target>`, nothing more; and **composite actions come
from `firestoned/github-actions`**, never inlined as direct action calls
(fix the version in that repo and bump the ref here).

## Compatibility patches

> **Historical.** The two notes that were here — a `serde < 1.0.157`
> `rename_all_fields` workaround and the matching `schemars` snake_case
> fallout — predate the edition 2024 / MSRV 1.88 toolchain (D-001) and no
> longer apply. Kept only so a reader who finds them referenced elsewhere
> knows they were resolved by the toolchain bump, not by a code workaround.

If a generated CRD ever shows snake_case where camelCase was expected, the
cause is a missing `#[serde(rename_all = "camelCase")]` on the type, not a
dependency-version problem — fix the type and rerun the `regen-crds` skill.

## Conventional commits

- `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`.
- Sign off every commit (`git commit -s`).
- Scope is the crate name: `feat(banlieue-controller): ...`.

## Pre-commit hook (optional)

```sh
#!/bin/sh
set -e
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all --no-fail-fast
```
