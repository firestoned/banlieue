# Guide: testing claim authorization with your real GitHub account

`VirtualMachineClaim.spec.subject` records *who* a sandbox VM was handed to,
and an admission policy requires it to name the person creating the claim
([ADR-0047](https://github.com/firestoned/banlieue/blob/main/docs/adr/0047-virtualmachineclaim.md)
Decision 10). On a stock `kind` cluster you are `kubernetes-admin`, so the
only identity you can test with is a certificate CN — which tells you the
CEL compiles but nothing about how the policy behaves for a real human.

This sets up a dev cluster that authenticates you **as your GitHub account**,
so you can watch the policy accept a claim naming you and refuse one naming
somebody else.

!!! tip "The flow this guide exercises, as a diagram"
    [The Claim Flow, End to End](../concepts/virtualmachine-claim-flow.md) §1 and §2 draw the
    `kubelogin` → Dex → GitHub round trip and the admission checks it feeds,
    which is useful to have open beside the commands below.

## GitHub cannot do this on its own

The first thing to know, because it saves an afternoon:

!!! warning "GitHub is an OAuth2 provider, not an OIDC provider"
    `kube-apiserver --oidc-issuer-url` needs an OpenID Connect issuer: a
    discovery document at `/.well-known/openid-configuration`, a JWKS, and
    an **ID token** carrying `iss`, `sub` and `aud`.

    GitHub serves none of those. It issues opaque access tokens for its own
    API. Pointing `--oidc-issuer-url` at `https://github.com` fails, and it
    fails in a way that looks like a certificate problem.

[Dex](https://dexidp.io/) bridges the gap: it authenticates you against
GitHub and mints a real OIDC ID token that the API server will accept. Dex
is the thing Kubernetes trusts; GitHub is the thing Dex trusts.

## The topology, and the one trick in it

```text
  browser ──┐
            ├─▶ https://127.0.0.1:32000/dex  ──▶  GitHub
  kubectl ──┘            ▲
                         │  the same URL, from inside the node
  kube-apiserver ────────┘
```

Dex runs in-cluster on NodePort 32000, and `kind` maps host port 32000 onto
the node. That gives **one** issuer URL reachable from the browser, from
`kubectl` on your laptop, and from the API server inside the node.

That matters because an OIDC issuer URL must match the token's `iss` claim
*exactly*. If the API server knew Dex as `https://dex.dex.svc:5556` and your
browser knew it as `https://127.0.0.1:32000`, the tokens would be rejected
for an issuer mismatch while everything looked correctly configured. One
spelling for all three consumers avoids the whole class of problem — and is
why the serving certificate needs an **IP SAN** for `127.0.0.1` rather than
a DNS name.

## Setup

### 1. A GitHub OAuth App

!!! tip "Google or Auth0 instead?"
    Both are real OIDC providers, so they can skip Dex entirely — and the
    redirect URI you register differs accordingly. Per-provider click-paths,
    connector YAML and gotchas:
    [OAuth / OIDC Clients](../developer/oauth-clients.md).

[github.com/settings/developers](https://github.com/settings/developers) →
**New OAuth App** (an OAuth App, *not* a GitHub App — they are different
things and only the former does the plain authorization-code flow Dex wants):

| Field | Value |
| --- | --- |
| Application name | anything, e.g. `banlieue dev` |
| Homepage URL | `https://127.0.0.1:32000/dex` |
| Authorization callback URL | `https://127.0.0.1:32000/dex/callback` |

GitHub permits `127.0.0.1` callbacks, which is what makes a laptop-only loop
possible. Generate a client secret, then:

```sh
export GITHUB_CLIENT_ID=Iv1.xxxxxxxxxxxx
export GITHUB_CLIENT_SECRET=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
```

### 2. `kubelogin`

The piece that turns `kubectl` into something that can run a browser flow:

```sh
kubectl krew install oidc-login
# or: brew install int128/kubelogin/kubelogin
```

### 3. The cluster

Two routes. **`up`** builds a fresh cluster with the OIDC flags baked in at
creation, which is the clean way:

```sh
make dev-oidc-up
```

That generates a throwaway CA, creates a `kind` cluster whose API server
trusts it, deploys Dex with the GitHub connector, installs banlieue's CRDs
and the claim policy, and grants `create virtualmachineclaims` to any
authenticated user.

**`attach`** retrofits the same wiring onto a cluster you already have —
worth it when that cluster has a `Provider`, a warm pool and real VMs on a
hypervisor, which is more valuable than a clean one and cannot be re-created
without losing it:

```sh
CLUSTER=banlieue-demo make dev-oidc-attach
```

`attach` copies the CA onto the node with `docker cp`, patches the API
server's static-pod manifest in place (the kubelet restarts it), and runs a
small **socat container** on kind's Docker network to publish
`127.0.0.1:32000` — standing in for the `extraPortMapping` a running cluster
cannot be given. The manifest is backed up to `kube-apiserver.yaml.pre-oidc`
first, and if the API server does not come back the script prints the one
command that restores it.

!!! note "`attach` restarts the API server"
    Watch streams break and reconnect, so controllers log a burst of
    `watch stream` errors and then recover. Objects, pools, claims and
    running VMs are untouched. Certificate auth keeps working alongside
    OIDC, so an admin kubeconfig — and any controller using one — is
    unaffected.

    The socat proxy (`<cluster>-dex-proxy`) is the one extra moving part. It
    targets the **NodePort**, not a pod, so Dex restarts do not affect it,
    and it carries `--restart unless-stopped` so it comes back with Docker.

    An earlier version used `kubectl port-forward` here, which binds to a
    *specific pod* and therefore died whenever Dex was replaced — including
    by `dev-oidc-github-creds`, making the documented sequence break itself.
    If you see `connection refused` on the issuer URL, check the proxy
    container before suspecting the cluster.

Either route ends by proving the part that actually matters:

```text
==> verifying OIDC discovery from inside the control-plane node
==>   ✓ the API server's node can fetch and trust https://127.0.0.1:32000/dex
```

A browser that reaches Dex proves nothing about the component doing the token
validation, so the check runs from inside the node.

### 4. Credentials, if you attached first

`attach` is worth running before you have an OAuth App, because it proves
every link except the GitHub redirect. When the app exists:

```sh
export GITHUB_CLIENT_ID=... GITHUB_CLIENT_SECRET=...
CLUSTER=banlieue-demo make dev-oidc-github-creds
```

That updates the secret and restarts Dex. Nothing about the cluster changes.

### 5. Log in

```sh
make dev-oidc-login
```

A browser opens, GitHub asks you to authorize the app, and `kubectl` ends up
holding an ID token. The command prints who you now are:

```text
==> You are 'oidc:octocat' to this API server.
==> A claim you create must set spec.subject.id to exactly that.
```

The username comes from Dex's `preferred_username` claim — your GitHub login
— with an `oidc:` prefix added by the API server. The login step also
rewrites the policy's issuer allowlist to this cluster's real issuer, since
the shipped value is a placeholder.

## Watch the policy decide

```sh
make dev-oidc-try-claim
```

Two claims, as you, one of them dishonest:

```text
==> 1/2 a claim naming YOU (oidc:octocat) — expect this to be accepted
virtualmachineclaim.banlieue.io/mine created (server dry run)

==> 2/2 the same claim naming SOMEBODY ELSE — expect Forbidden
Error from server (Forbidden): ... denied request: spec.subject.id is
oidc:somebody-else but you are authenticated as oidc:octocat. A claim
records who a sandbox VM was handed to, so it may only name its own
creator — otherwise the audit trail says whatever the author typed.
```

That is the whole point of the policy, seen against a real identity rather
than a certificate CN.

### Worth trying by hand

```sh
# The bypass the immutability rule exists for: create honestly, then patch.
kubectl --context banlieue-oidc apply -f - <<EOF
apiVersion: banlieue.io/v1alpha1
kind: VirtualMachineClaim
metadata: { name: mine, namespace: banlieue-system }
spec:
  poolRef: { name: sandbox-pool }
  subject: { issuer: "https://127.0.0.1:32000/dex", id: "oidc:octocat" }
  ttlSeconds: 900
EOF

kubectl --context banlieue-oidc -n banlieue-system \
  patch vmclaim mine --type=merge \
  -p '{"spec":{"subject":{"issuer":"https://127.0.0.1:32000/dex","id":"oidc:someone"}}}'
# Forbidden: spec.subject is immutable
```

```sh
# An issuer this cluster does not trust.
#   → Forbidden: spec.subject.issuer ... is not in this cluster's allowlist
```

## Becoming a broker

A **broker** is a service account whose job is handing sandboxes to *other*
people — the role roadmap 17 phase C describes. Brokers are exempt from the
id check, because a broker that could only name itself could not broker.

```sh
kubectl -n banlieue-system patch configmap banlieue-claim-subject-policy \
  --type=merge -p '{"data":{"brokers":"oidc:octocat\n"}}'
```

Re-run `make dev-oidc-try-claim` and the second claim is now **accepted** —
you may attribute a sandbox to anyone. Re-running the patch attempt above
still fails, because immutability applies to brokers too: attribution is
fixed at the moment it was authorized.

!!! danger "That ConfigMap is the trust concentration"
    Anyone on the `brokers` list can attribute a sandbox to any identity, so
    the list is as sensitive as the audit trail it underwrites. It is empty
    by default. In a real cluster, restrict who can edit it and alert on
    changes.

## What this does *not* test

Two limits worth being explicit about, because the setup looks more complete
than it is:

- **The issuer is allowlisted, never verified.** The API server does not
  reveal which issuer minted the caller's token, so no admission policy can
  check it. The allowlist only stops a claim naming an issuer your site does
  not use. Verifying the subject's *token* is the in-guest agent's job
  (roadmap 17 phase C), against `status.nonce`.
- **Whether a VM is involved depends on the route.** `make dev-oidc-try-claim`
  uses `--dry-run=server`, which exercises admission and stops there — the
  right scope for a cluster built by `up`, which has no `Provider`. On a
  cluster you reached with `attach`, drop the `--dry-run=server` and the
  claim binds a **real** member from the existing pool, so you get the whole
  path in one place: GitHub login → admission → bind → a VM on a hypervisor.
  See [VirtualMachine Pools](virtualmachine-pools.md).

## Teardown

```sh
make dev-oidc-down
```

Deletes the cluster, the kubectl context and user, and the throwaway CA.
The CA is dev-only but is still key material, so it is removed rather than
left in `$TMPDIR`; the script verifies the cluster is really gone rather
than assuming.

You can also delete the GitHub OAuth App when you are finished with it —
nothing else depends on it.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| `oidc-issuer-url` unreachable, or the API server rejects every token | Dex is not running, or the issuer URL does not match `iss` exactly. `make dev-oidc-status` shows the Dex pod; the URL must be byte-identical everywhere. |
| `x509: certificate signed by unknown authority` from the API server | The CA reached the node but not the API server *pod*. It needs both a kind `extraMounts` (host→node) and a kubeadm `apiServer.extraVolumes` (node→static pod); the script sets both. |
| `redirect_uri_mismatch` from GitHub | The OAuth App's callback URL is not exactly `https://127.0.0.1:32000/dex/callback`. |
| Browser warns about the certificate | Expected — it is a throwaway CA your browser has never seen. `kubectl` trusts it explicitly via `--certificate-authority`. |
| `connection refused` fetching the issuer's discovery document | The socat proxy is not running. `make dev-oidc-status` shows it; re-run `dev-oidc-attach` to recreate it. |
| `unknown command "email" for "kubelogin get-token"` | A comma-separated `--exec-arg` in the kubeconfig. `kubectl config set-credentials` splits `--exec-arg` on commas, so `--oidc-extra-scope=a,b,c` becomes three arguments and the bare ones read as subcommands. One `--exec-arg` per scope. |
| Every claim is Forbidden on the issuer check | The policy's `issuers` list still holds the shipped placeholder. `make dev-oidc-login` rewrites it; otherwise patch the ConfigMap. |
| `Unauthorized` / `invalid bearer token` some time after a login that worked | Your ID token was signed by a Dex signing key that no longer exists. Dex keeps keys in its storage, so this should not happen — unless Dex was redeployed with `storage: type: memory`, which regenerates keys on every start. Log in again. |
| `kubectl auth whoami` shows a long base64 string | Your Dex is mapping `sub` rather than `preferred_username`. The username has to be typed into `spec.subject.id` by a human, which is why this setup maps the GitHub login. |

## Reference

- [ADR-0047 — `VirtualMachineClaim`](https://github.com/firestoned/banlieue/blob/main/docs/adr/0047-virtualmachineclaim.md), Decision 10
- `deploy/admission/virtualmachineclaim-subject-authorization.yaml`
- `scripts/dev-oidc-kind.sh`
- [Dex GitHub connector](https://dexidp.io/docs/connectors/github/)
- [kubelogin](https://github.com/int128/kubelogin)
- [VirtualMachine Claims guide](virtualmachine-claims.md)
