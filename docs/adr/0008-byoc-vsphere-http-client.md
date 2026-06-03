<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0008 — Bring-Your-Own-Client (BYOC) for the vSphere HTTP transport

- **Status:** Accepted
- **Date:** 2026-06-01
- **Deciders:** Erick Bourgeois
- **Related:** ADR-0006 (release & supply-chain pipeline); ADR-0003 (provider deployment topology); `crates/banlieue-provider-vsphere/src/client/vim.rs`; `ProviderConnection` (`crates/banlieue-api/src/banlieue/provider.rs`); the vendored `vim_rs` build-time patch (`patches/vim_rs.patch`, Makefile `vendor-vim-rs`); upstream [noclue/vim_rs#37](https://github.com/noclue/vim_rs/issues/37).

> **Update (2026-06-03):** the build-side caveat below — that BYOC does **not**
> retire the vendoring pipeline, because removing OpenSSL still depended on the
> `vim_rs` reqwest features — is now **resolved upstream**. vim_rs 0.5.0 ships
> reqwest 0.13/rustls and a first-class BYOC mode (#37). **ADR-0009** records the
> migration to 0.5, the reqwest 0.13 + ring move, and the deletion of the
> patch/vendoring pipeline and libssl scaffolding. Read this ADR for the BYOC
> *runtime* design; read ADR-0009 for the *build* end state.

## Context

The vSphere provider talks to vCenter through [`vim_rs`](https://github.com/noclue/vim_rs),
which builds its HTTP transport on `reqwest`. Two separate concerns have become
tangled and need to be separated explicitly.

### Concern A — who owns the HTTP client (runtime)

Today `VimClientFactory::build` lets `vim_rs` construct the `reqwest::Client`
for us (`vim.rs:51`):

```rust
ClientBuilder::new(&connection.endpoint)
    .basic_authn(&creds.username, &creds.password)
    .app_details(APP_NAME, APP_VERSION)
    .insecure(connection.insecure_skip_tls_verify)   // vim_rs builds the client
    .build().await?
```

`vim_rs`'s built-in builder exposes only a coarse `insecure` toggle. Its
`build_json` path (`vim_rs/src/core/client.rs:457`) honours **only**
`danger_accept_invalid_certs` / `danger_accept_invalid_hostnames` — it has no
hook for a custom CA bundle, client certificates (mTLS), connection/read
timeouts, proxy configuration, or HTTP/2 tuning. The consequence shows in our
own API surface: `ProviderConnection.ca_bundle` (an `Option<String>` PEM bundle,
`provider.rs:115`) **exists but is silently ignored** — there is no way to feed
it through `.insecure(bool)`. An operator who sets `caBundle` to pin a private
CA gets no effect; their only knob is the blunt, insecure "skip verification"
flag. That violates the project's least-privilege / explicit-over-implicit
posture: TLS trust should be declared and enforced, not bypassed.

`vim_rs` already provides the seam to fix this. `ClientBuilder::http_client`
(`client.rs:391`) accepts a fully-built `reqwest::Client`; `build_json` uses it
verbatim when present and only falls back to constructing its own when it is
`None`. `http_client()` and `insecure()` are mutually exclusive by
construction (each resets the other). So the caller can own the entire transport
configuration with **no fork and no patch** — it is a supported, public API.

### Concern C — how the CA bundle is supplied (CRD ergonomics)

An inline PEM string is the simplest source but not the only one operators want.
A cluster-wide trust bundle is commonly distributed as a **ConfigMap** (the
`kube-root-ca.crt`-style pattern, or a corporate-PKI bundle managed centrally); a
private CA whose material is sensitive may live in a **Secret**. Forcing the PEM
inline into every `Provider` spec means copying and rotating it by hand in N
places. So `caBundle` must accept a **value-or-source**: inline PEM, **or** a
reference to a ConfigMap key, **or** a reference to a Secret key — exactly one.
This mirrors how `credentialsRef` already points at a Secret rather than
inlining the password.

### Concern B — which TLS backend is compiled in (build / supply chain)

This is orthogonal to A and is frequently conflated with it. The TLS backend is
selected by `reqwest`'s **compile-time Cargo features**, not at
`Client::builder()` runtime. Unpatched `vim_rs` declares `reqwest = "0.12"` with
default features, which pulls reqwest's `default-tls` → **native-tls → OpenSSL**.
Verified empirically: reversing our local patch and re-resolving adds
`openssl v0.10.80`, `native-tls`, and `hyper-tls` to the graph.

Because Cargo features are **additive across the whole graph**, a downstream
crate cannot subtract OpenSSL by choosing rustls for itself: if `vim_rs` keeps
default-tls and banlieue adds `rustls`, the resolver takes the **union** and
OpenSSL stays in the tree. The only ways to keep OpenSSL out of the build are
(a) change `vim_rs`'s own `reqwest` features, or (b) have `vim_rs` expose a
backend-selection feature.

We currently do (a) via `patches/vim_rs.patch` — a one-line edit setting
`reqwest = { default-features = false, features = ["rustls-tls-native-roots",
"charset", "http2"] }`, applied to a vendored checkout at build time
(Makefile `vendor-vim-rs`, wired into every cargo target and CI). This is being
upstreamed as **noclue/vim_rs#37** ("Make the TLS backend selectable"), whose
scope is *strictly* TLS-backend selection — no certificate-handling, auth, or
`vcsim_compat` behavioural changes.

> **Correction of an earlier working assumption.** It was briefly believed that
> 0.4.4 already shipped rustls; that reading was of the *patched* working tree.
> Unpatched upstream 0.4.4 still uses OpenSSL. BYOC does **not** remove OpenSSL
> from the build — only the patch (or #37 landing) does. The two concerns must
> be decided independently.

## Decision

**Adopt BYOC for the vSphere transport: banlieue constructs the
`reqwest::Client` and injects it via `ClientBuilder::http_client(...)`, owning
all TLS and transport policy. Make `caBundle` a value-or-source (inline PEM /
ConfigMap ref / Secret ref). Keep the `vim_rs` rustls patch as a separate,
build-time concern until noclue/vim_rs#37 ships upstream.**

### CRD shape

`ProviderConnection.ca_bundle` changes from `Option<String>` to
`Option<CABundleSource>` (new type in `crates/banlieue-api`):

```rust
/// Source of a PEM-encoded CA bundle: exactly one of the three.
pub struct CABundleSource {
    /// Inline PEM (one or more concatenated certificates).
    pub inline: Option<String>,
    /// Key in a ConfigMap in the Provider's namespace. Key defaults to `ca.crt`.
    pub config_map_ref: Option<KeySelector>,
    /// Key in a Secret in the Provider's namespace. Key defaults to `ca.crt`.
    pub secret_ref: Option<KeySelector>,
}

/// Reference to a single key within a named object in the same namespace.
pub struct KeySelector {
    pub name: String,
    /// Defaults to `ca.crt` when omitted.
    pub key: Option<String>,
}
```

Default key **`ca.crt`** matches Kubernetes' own convention (`kube-root-ca.crt`,
service-account CA, webhook `caBundle` all key on `ca.crt`). All three sources
are namespace-local — the bundle lives alongside the `Provider`, like its
`credentialsRef` Secret. (`KeySelector` is generic enough to reuse for future
key-scoped references; it lives in `common.rs` next to `LocalObjectReference`.)

### Exactly-one enforcement (defense in depth)

- **Controller-side (always on):** the provider resolves the bundle in its
  reconcile path; zero sources is "no bundle" (use system roots), and **more
  than one** set is a hard `status` error (`Provider` not Ready, actionable
  message). This is the floor — it works on every cluster, mirroring how
  `read_credentials` already fails closed on a missing Secret.
- **Admission (VAP, optional):** a `ValidatingAdmissionPolicy` in
  `deploy/admission/` (per ADR-0007) rejects a spec with ≠1 source at write
  time. Stronger and earlier, but optional / K8s 1.30+, so it hardens rather
  than replaces the controller-side check.

### Resolution + client build (in `VimClientFactory::build`)

```rust
// resolve_ca_bundle: inline → use as-is; configMapRef → read ConfigMap[key];
// secretRef → read Secret[key]; key defaults to "ca.crt"; >1 source or a
// missing object/key → Error (surfaced on Provider.status, like credentials).
let pem: Option<String> = resolve_ca_bundle(ctx, namespace, &connection.ca_bundle).await?;

let mut rb = reqwest::Client::builder()
    .user_agent(format!("{APP_NAME}/{APP_VERSION}"));   // was app_details()
if let Some(pem) = &pem {
    // from_pem_bundle (not from_pem): a caBundle may carry a chain / multiple
    // CAs; from_pem would silently take only the first certificate.
    for cert in reqwest::Certificate::from_pem_bundle(pem.as_bytes())? {
        rb = rb.add_root_certificate(cert);
    }
}
if connection.insecure_skip_tls_verify {
    rb = rb.danger_accept_invalid_certs(true)
           .danger_accept_invalid_hostnames(true);
}
let http = rb.build().map_err(/* → Error::Vsphere */)?;

ClientBuilder::new(&connection.endpoint)
    .http_client(http)                                   // BYOC
    .basic_authn(&creds.username, &creds.password)
    .build().await?
```

Rules this establishes:

- **banlieue owns the transport.** The provider builds and configures the
  `reqwest::Client`; `vim_rs` never constructs one for us. `app_details` moves
  onto the reqwest `user_agent` (its only effect was the User-Agent header).
- **`caBundle` is honoured, from any of three sources.** Resolved PEM is added as
  root certificate(s) — the first time `ProviderConnection.ca_bundle` has any
  effect. This is the secure path; `insecureSkipTLSVerify` remains a separate,
  loud, opt-in escape hatch (the two are independent — a CA bundle does not imply
  skipping verification).
- **Reads ConfigMaps now, not just Secrets.** Resolving a `configMapRef` requires
  adding `configmaps` (`get`/`list`/`watch`) to the vsphere provider ClusterRole;
  Secret read is already granted. Least-privilege: read-only, no write verbs.
- **TLS backend stays a build-time concern.** banlieue's transport policy is
  backend-agnostic; whether OpenSSL or rustls is linked is governed by the
  `vim_rs` reqwest features, addressed by the patch / #37 — **not** by this
  change. BYOC neither requires nor removes the patch.

This is recorded as architecturally significant because it moves a security
boundary (TLS trust establishment for every vCenter call) from a third-party
dependency into banlieue, broadens the provider's RBAC to ConfigMaps, and
clarifies the contract between the provider and `vim_rs`.

## Consequences

**Positive**

- **`caBundle` works.** Private-CA / corporate-PKI vCenter endpoints can be
  validated properly instead of forcing operators to disable verification.
  Closes a real least-privilege gap.
- **Flexible bundle sourcing.** Inline for quick use; ConfigMap ref for a
  centrally-managed corporate trust bundle (rotate in one place, many Providers
  pick it up); Secret ref for sensitive CA material. No copy-paste of PEM into
  every spec.
- **Explicit, auditable transport policy.** CA trust, optional mTLS, timeouts,
  and proxy settings live in banlieue's code, reviewable in one place, rather
  than being implicit in a dependency's defaults.
- **No new fork or patch.** BYOC uses `vim_rs`'s public `http_client()` API; it
  adds nothing to the vendoring pipeline.
- **Cleaner seam for testing.** The provider can inject a client pointed at a
  fixture/proxy without `vim_rs` reaching out on its own.

**Negative / trade-offs**

- **Does not retire the vendoring pipeline.** This was an attractive hope and is
  explicitly *not* delivered: removing OpenSSL from the build still depends on
  the `vim_rs` reqwest features (patch today, #37 upstream later). The
  `vendor-vim-rs` / `[patch.crates-io]` / stamp machinery stays until #37 lands.
  Tracked so the two are not re-conflated.
- **banlieue now owns TLS correctness.** Bugs in our client construction (e.g.
  forgetting to add the CA bundle, mis-handling PEM) are ours, not `vim_rs`'s.
  Mitigated by unit tests around `VimClientFactory::build` and by keeping the
  construction small and explicit.
- **Backend choice is implicit at the call site.** `reqwest::Client::builder()`
  uses whatever TLS backend the compiled-in features provide; the BYOC code does
  not (and cannot) assert "rustls" at runtime. We rely on the build (patch/#37)
  for that, and should add a `cargo-deny`/graph check that OpenSSL is absent so a
  regression is caught in CI rather than silently relinked.
- **Two TLS verification paths conceptually.** `caBundle` (secure) and
  `insecureSkipTLSVerify` (bypass) both exist; precedence must be documented
  (insecure wins only when explicitly set; otherwise system roots + any
  `caBundle` apply).
- **Wider RBAC + a new failure mode.** The provider now also reads ConfigMaps
  (read-only), a small surface increase. And resolution can fail in new ways —
  referenced ConfigMap/Secret or key absent, >1 source set — each surfaced as a
  `Provider` status error rather than a silent fallback to system roots, so a
  misconfigured trust ref fails closed.
- **Breaking schema change (pre-GA).** `caBundle` goes from a string to an
  object. Acceptable at v1alpha1 and there are no known inline users yet, but it
  is a shape change to a published CRD; called out in the changelog.

## Alternatives considered

- **Patch / fork `vim_rs` to take a CA bundle through its own builder.** More
  code in the dependency we don't own, duplicating what `http_client()` already
  enables, and growing the patch surface we're trying to shrink. Rejected:
  BYOC is the supported, fork-free seam.
- **Rely on `insecureSkipTLSVerify` only (status quo).** Forces operators to
  disable verification to reach private-CA vCenters — a least-privilege
  violation and a security footgun. Rejected.
- **Keep `caBundle` an inline string only.** Simplest, no new RBAC. Rejected: a
  centrally-managed corporate trust bundle would have to be copy-pasted and
  rotated into every `Provider` spec; ConfigMap/Secret refs are the idiomatic
  K8s way and match `credentialsRef`'s existing indirection.
- **One ref field of a tagged kind (`kind: ConfigMap|Secret`) instead of two.**
  More compact, but loses static typing and makes the "exactly one of
  inline/ref" check no simpler. The three-optional-fields shape mirrors
  upstream patterns (e.g. envFrom sources) and reads clearly in YAML. Minor;
  chosen for explicitness.
- **Treat BYOC as the way to drop OpenSSL / retire vendoring.** The original
  motivation, shown false above: Cargo's additive features mean a
  banlieue-side rustls client cannot evict OpenSSL while `vim_rs` keeps
  default-tls. BYOC and the TLS-backend patch are independent; conflating them
  would leave OpenSSL silently linked. Rejected as a basis for retiring the
  pipeline.
- **Wait for #37 upstream, then plain `vim_rs = "0.4.x"` + BYOC.** The clean end
  state. We adopt BYOC now (independent of #37) and retire the vendoring
  pipeline when #37 releases — at which point this ADR's build-side caveat is
  resolved by bumping the dependency and deleting the patch (per
  `patches/README.md`).
