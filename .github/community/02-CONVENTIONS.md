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

- **Unit tests** live next to code in `#[cfg(test)] mod tests`.
- **Integration tests** live in `tests/`; use the `kube` mock harness
  or stand up a `kind` cluster.
- For reconcilers, prefer **table-driven tests** that exercise:
  - Happy path (create → ready)
  - Scheduling failure (no candidate)
  - Status mirroring (infra goes ready → VM goes ready)
  - Deletion via finalizer
- Mock external clients behind a trait so tests don't need a real
  vCenter / Proxmox / libvirt.

## Webhooks

- One binary per webhook role (`banlieue-validating-webhook`,
  `banlieue-mutating-webhook`).
- Use `kube` runtime support and serve over HTTPS with cert
  injection via cert-manager.
- Defaulting fills in `firmware`, `migrationPolicy`,
  `desiredPowerState`, `provisioning`, IPAM `source` defaults.
- Validation enforces immutability and reference consistency.

## Container images

- Multi-stage: build in `rust:1.80` image, copy binary to distroless
  runtime.
- Set `USER nonroot:nonroot`.
- `WORKDIR /` and run from there; binary at `/banlieue-controller`,
  `/banlieue-provider-vsphere`, etc.
- Health check: `/healthz` (liveness) and `/readyz` (readiness) on
  port 8081 in every binary.

## CI (GitHub Actions, target shape)

- `fmt`, `clippy`, `test` on every PR.
- `crdgen` runs and verifies the output matches `deploy/crds/`
  (regenerate locally if it doesn't; CI fails otherwise).
- E2E in Phase 4.
- DCO check via [DCO app](https://github.com/apps/dco).

## Compatibility patches

### serde `rename_all_fields`

If `cargo check -p banlieue-api` complains about `rename_all_fields`,
your serde is < 1.0.157. Either:

- Bump `serde = "1.0.157"` minimum in the workspace `Cargo.toml`, or
- Remove `rename_all_fields = "camelCase"` from `IpamSpec` in
  `common.rs` and add explicit `#[serde(rename = "poolRef")]` on the
  `pool_ref` field of the `Pool` variant.

### schemars output names

If the generated CRD shows snake_case field names where camelCase was
expected, `schemars` isn't propagating `rename_all_fields`. Apply the
explicit rename fix above.

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
