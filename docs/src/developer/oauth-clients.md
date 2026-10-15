# Setting up OAuth / OIDC clients

banlieue's claim subject policy binds `VirtualMachineClaim.spec.subject.id` to
the identity the API server authenticated
([ADR-0047](https://github.com/firestoned/banlieue/blob/main/docs/adr/0047-virtualmachineclaim.md)
Decision 10). To exercise that against a real person rather than a
certificate CN, the cluster needs an identity provider.

This page is the provider-side setup for **GitHub**, **Google** and **Auth0**:
what to click, what to register, and what username you end up with. The
cluster-side plumbing is [Testing claim
authorization](../guides/testing-claim-authorization.md).

## Decide first: do you need Dex?

This is the fork everything else hangs off, and getting it wrong wastes an
afternoon.

| Provider | An OIDC provider? | Needs Dex? |
| --- | --- | --- |
| **GitHub** | ❌ No — OAuth2 only | **Yes** |
| **Google** | ✅ Yes (`https://accounts.google.com`) | Optional |
| **Auth0** | ✅ Yes (`https://<tenant>.<region>.auth0.com/`) | Optional |

`kube-apiserver --oidc-issuer-url` requires an OpenID Connect issuer: a
discovery document at `/.well-known/openid-configuration`, a JWKS, and an
**ID token** carrying `iss` / `sub` / `aud`.

- **GitHub serves none of those.** It issues opaque access tokens for its own
  REST API. Pointing the API server at `https://github.com` fails, and it
  fails looking like a TLS problem. [Dex](https://dexidp.io/) authenticates
  against GitHub and mints a real ID token on its behalf.
- **Google and Auth0 are genuine OIDC providers**, so the API server can trust
  them directly and Dex becomes optional. Use Dex anyway if you want one
  issuer in front of several providers, or GitHub alongside them.

## The redirect URI is what people get wrong

Register the redirect URI of **whichever component receives the callback** —
and that differs between the two topologies:

| Topology | Register this redirect URI |
| --- | --- |
| **Via Dex** | `https://127.0.0.1:32000/dex/callback` — Dex's callback |
| **Direct to the API server** | `http://localhost:8000` **and** `http://localhost:18000` — `kubelogin`'s local listener |

With Dex, the provider never talks to `kubectl`; it talks to Dex, and Dex
talks to the API server. Registering `kubelogin`'s ports in a Dex setup (or
Dex's callback in a direct setup) produces `redirect_uri_mismatch`, which is
the single most common failure here.

`kubelogin` binds port 8000 and falls back to 18000, so register both.

## Which username you end up with

This matters more for banlieue than for most things, because whatever the API
server calls you is what must be typed into `spec.subject.id`.

| Provider | Sensible `--oidc-username-claim` | You become |
| --- | --- | --- |
| GitHub (via Dex) | `preferred_username` | `github:octocat`, the GitHub login |
| Google (direct) | `email` | `google:you@example.com` |
| Auth0 (direct) | `email` | `auth0:you@example.com`; Auth0 issues no `preferred_username` |

Behind Dex, every upstream comes out of the one issuer with Dex's prefix
(`github:` in banlieue's scripts). Trusted directly, each provider is its
own issuer and gets its own prefix: see [All three on one
cluster](#all-three-on-one-cluster) for why that is not optional.

!!! warning "Do not use `sub` as the username claim"
    It is the spec-correct stable identifier, and it is unusable here.
    Google's `sub` is a 21-digit number; Dex's is opaque base64. Both are
    correct and neither can be typed into a YAML field by a human, so the
    claim's whole audit purpose gets lost in transcription errors.

    `email` and `preferred_username` are mutable at the provider, which is
    the trade-off. For a dev loop that is the right side of it.

---

## GitHub

GitHub requires Dex. Create an **OAuth App** — *not* a GitHub App; they are
different products and only the former does the plain authorization-code
flow Dex expects.

### 1. Create the app

[github.com/settings/developers](https://github.com/settings/developers) →
**OAuth Apps** → **New OAuth App**

| Field | Value |
| --- | --- |
| Application name | anything, e.g. `banlieue dev` |
| Homepage URL | `https://127.0.0.1:32000/dex` |
| Authorization callback URL | `https://127.0.0.1:32000/dex/callback` |

Then **Generate a new client secret** and copy it immediately — GitHub shows
it once.

GitHub permits `127.0.0.1` and `localhost` callbacks, which is what makes a
laptop-only loop possible with no public DNS.

### 2. Dex connector

```yaml
connectors:
  - type: github
    id: github
    name: GitHub
    config:
      clientID: $GITHUB_CLIENT_ID
      clientSecret: $GITHUB_CLIENT_SECRET
      redirectURI: https://127.0.0.1:32000/dex/callback
      # Restrict who may log in. Omit entirely to allow any GitHub account,
      # which is reasonable for a laptop cluster and not otherwise.
      # orgs:
      #   - name: my-org
      #     teams: [platform]
      loadAllGroups: false
      teamNameField: slug
```

### GitHub-specific notes

- **Org membership must be public, or grant the app org access.** With
  `orgs:` set, Dex reads your organisation membership — private membership is
  invisible unless the org has approved the OAuth App. A user in the org who
  appears not to be is almost always this.
- **Groups are `org:team` strings**, e.g. `my-org:platform`, only when
  `orgs[].teams` is set or `loadAllGroups: true`.
- **An OAuth App is owned by a user or an org**, and org-owned apps may need
  owner approval before anyone can use them.

---

## Google

Google is a real OIDC provider, so you can go through Dex or direct.

### 1. Configure the consent screen

Google will not issue a client ID until a consent screen exists.

[Google Cloud Console](https://console.cloud.google.com/) → select or create
a project → **APIs & Services** → **OAuth consent screen**

- **User type**: *Internal* if you have Google Workspace and only your own
  domain needs to log in — no verification and no test-user list. *External*
  otherwise, which starts in "Testing" mode.
- **Scopes**: `openid`, `email`, `profile` are enough. Nothing sensitive, so
  no verification review.
- **Test users** (External + Testing only): add every account that will log
  in. Omitting this is the usual cause of "you do not have access".

### 2. Create the client

**APIs & Services** → **Credentials** → **Create credentials** → **OAuth
client ID**

| Field | Value |
| --- | --- |
| Application type | **Web application** |
| Authorized redirect URIs (via Dex) | `https://127.0.0.1:32000/dex/callback` |
| Authorized redirect URIs (direct) | `http://localhost:8000`, `http://localhost:18000` |

!!! note "Web application, even for a CLI"
    "Desktop app" issues a client that cannot use a client secret, and both
    Dex and `kubelogin`'s default flow expect one. Web application with
    localhost redirect URIs is the right choice, and Google explicitly
    permits `localhost`.

### 3a. Via Dex

```yaml
connectors:
  - type: google
    id: google
    name: Google
    config:
      clientID: $GOOGLE_CLIENT_ID
      clientSecret: $GOOGLE_CLIENT_SECRET
      redirectURI: https://127.0.0.1:32000/dex/callback
      # Workspace only: refuse anyone outside these domains.
      # hostedDomains: [example.com]
```

### 3b. Direct to the API server

```text
--oidc-issuer-url=https://accounts.google.com
--oidc-client-id=<your-client-id>.apps.googleusercontent.com
--oidc-username-claim=email
--oidc-username-prefix=google:
```

The flags allow one issuer. To trust Google beside Dex or Auth0, use a
structured authentication file instead: [All three on one
cluster](#all-three-on-one-cluster).

No `--oidc-ca-file`: Google's certificate chains to a public root the API
server already trusts. That is the main practical advantage of going direct.

### Google-specific notes

!!! warning "Google does not support the OIDC `groups` claim"
    There is no scope that returns group membership. Dex can fetch it only
    via a **service account with domain-wide delegation** — Admin SDK API
    enabled, delegated
    `https://www.googleapis.com/auth/admin.directory.group.readonly`, and a
    `domainToAdminEmail` mapping to an admin with at least *Groups Reader*.

    For banlieue's claim policy this does not matter: the policy checks
    `subject.id` against the username, not groups. Only reach for the
    service account if you want group-based RBAC.

- **`email_verified` is absent for some Workspace accounts.** Dex rejects
  unverified emails by default; `insecureSkipEmailVerified: true` is the
  escape hatch, and the name is accurate about what you are giving up.
- **A "Testing" External app expires consent every 7 days**, so logins start
  failing a week later for no visible reason. Publish the app or use
  Internal.

---

## Auth0

Auth0 is a real OIDC provider. Dex reaches it through the **generic `oidc`
connector**, not a dedicated one.

Read the trailing-slash note below before configuring anything — it is the
one thing that reliably breaks Auth0 setups.

### 1. Create the application

Auth0 Dashboard → **Applications** → **Applications** → **Create
Application**

- **Name**: anything, e.g. `banlieue dev`
- **Type**: **Regular Web Applications**. "Single Page Application" and
  "Native" are public clients that issue no usable client secret, and both
  Dex and `kubelogin`'s default flow expect one.

Then on the application's **Settings** tab:

| Field | Value |
| --- | --- |
| Allowed Callback URLs (via Dex) | `https://127.0.0.1:32000/dex/callback` |
| Allowed Callback URLs (direct) | `http://localhost:8000`, `http://localhost:18000` |
| Allowed Logout URLs | may be left empty |

Auth0 calls them *Allowed Callback URLs* rather than redirect URIs; the
field takes a comma-separated list. **Domain**, **Client ID** and **Client
Secret** are on the same tab.

Your domain is usually `<tenant>.<region>.auth0.com` — e.g.
`dev-a1b2c3d4.us.auth0.com` — not the bare `<tenant>.auth0.com` that older
documentation shows.

### 2. The trailing slash

!!! danger "Auth0's issuer ends in `/`, and it has to be configured that way"
    Auth0's discovery document reports:

    ```json
    { "issuer": "https://dev-a1b2c3d4.us.auth0.com/" }
    ```

    Note the trailing slash. Kubernetes compares `--oidc-issuer-url` against
    the token's `iss` claim **byte-for-byte**, so it must be configured
    *with* the slash. Drop it and every token is rejected as an issuer
    mismatch, while the discovery URL you typed into a browser works
    perfectly.

    The confusion is that OIDC Discovery builds its URL by *removing* a
    terminating slash and appending `/.well-known/openid-configuration`, so
    the two differ by exactly one character and each is correct in its own
    place. This has bitten
    [go-oidc](https://github.com/coreos/go-oidc/issues/66) — which is what
    the API server uses — and
    [Istio](https://github.com/istio/istio/issues/45546) among others.

Settle it by reading the value back rather than reasoning about it:

```sh
curl -s https://dev-a1b2c3d4.us.auth0.com/.well-known/openid-configuration \
  | jq -r .issuer
# https://dev-a1b2c3d4.us.auth0.com/     <- configure exactly this
```

### 3a. Via Dex

```yaml
connectors:
  - type: oidc
    id: auth0
    name: Auth0
    config:
      # Exactly what discovery reported, trailing slash included.
      issuer: https://dev-a1b2c3d4.us.auth0.com/
      clientID: $AUTH0_CLIENT_ID
      clientSecret: $AUTH0_CLIENT_SECRET
      redirectURI: https://127.0.0.1:32000/dex/callback
      scopes: [openid, profile, email]
      getUserInfo: true
      # Auth0 issues no `preferred_username`; email is the usable identity.
      userNameKey: email
      # Only needed if you added the Action in step 4.
      # insecureEnableGroups: true
      # claimMapping:
      #   groups: "https://banlieue.io/groups"
```

### 3b. Direct to the API server

```text
--oidc-issuer-url=https://dev-a1b2c3d4.us.auth0.com/
--oidc-client-id=<your-client-id>
--oidc-username-claim=email
--oidc-username-prefix=auth0:
```

For the direct topology, a **Native** application is the better fit than a
Regular Web Application: it is a public client, so nothing secret ends up
in a kubeconfig, and `kubelogin` uses PKCE on its own because Auth0's
discovery advertises `S256`. Allowed Callback URLs are the same
`http://localhost:8000, http://localhost:18000`. Keep the Regular Web
Application for Dex, which does hold a secret.

As with Google, the flags allow one issuer; beside others, use [All three on
one cluster](#all-three-on-one-cluster).

No `--oidc-ca-file`: Auth0's certificate chains to a public root the API
server already trusts.

### 4. Groups, if you want them

!!! warning "Auth0 silently drops non-namespaced custom claims"
    A claim called `groups` will not appear in the token. Auth0 requires
    custom claims to be **namespaced URIs** — the namespace does not have to
    resolve, it just has to look like a URL. Adding a bare `groups` claim
    produces a token without it and no error anywhere, which reads as a Dex
    or API server fault.

Auth0 emits no group claim by default. Add one with a post-login **Action**:

**Actions** → **Library** → **Build Custom** → trigger *Login / Post Login*

```js
exports.onExecutePostLogin = async (event, api) => {
  const ns = 'https://banlieue.io/groups';
  api.idToken.setCustomClaim(ns, event.authorization?.roles ?? []);
};
```

Deploy it, then drag it into **Actions → Flows → Login** — an Action that is
not in a flow never runs, which is the other half of this trap.

Then uncomment `insecureEnableGroups` and `claimMapping` above, or for the
direct topology set `--oidc-groups-claim=https://banlieue.io/groups`.

For banlieue's claim policy none of this is required: the policy checks
`subject.id` against the username, not groups. Add the Action only if you
want group-based RBAC.

### Auth0-specific notes

- **The client secret is on the Settings tab** and can be re-read at any
  time, unlike GitHub's, which shows it once.
- **Auth0 requires HTTPS callbacks** except for `localhost` and `127.0.0.1`.
- **A fresh tenant has no database connection users.** Either create one
  under *User Management*, or enable a social connection and log in with
  that.
- **`insecureEnableGroups` is about staleness, not safety.** Groups refresh
  only when the ID token does; the name warns about the lag.

---

## All three on one cluster

`--oidc-*` flags configure exactly one issuer. To accept GitHub (through
Dex), Google and Auth0 at the same time, kube-apiserver needs a **structured
authentication configuration**: one file, passed as
`--authentication-config`, with one `jwt` entry per issuer
(`apiserver.config.k8s.io/v1`). The API server re-reads the file when it
changes, so adding or editing an issuer restarts nothing.

```text
  kubelogin ──▶ Dex ──▶ GitHub       ─┐
  kubelogin ──▶ accounts.google.com  ─┼─▶ ID token ──▶ kube-apiserver
  kubelogin ──▶ <tenant>.auth0.com   ─┘    (one jwt entry per issuer)
```

The API server picks the entry whose `issuer.url` equals the token's `iss`,
checks the signature against that issuer's keys and the `aud` against that
entry's `audiences`, then maps claims to a username.

### Each issuer needs its own username prefix

!!! danger "Never share a prefix between issuers, and never leave one empty"
    The prefix is the **only** part of an authenticated username that says
    which issuer vouched for it. RBAC, audit logs and banlieue's claim
    subject policy all see `request.userInfo.username` and nothing else.

    With one prefix for all three, `github:alice@example.com` could have
    been minted by Google for whoever registered that address there, or by
    an Auth0 database connection that let anyone sign up with it. A
    RoleBinding written for one person would then admit someone else.
    Distinct prefixes (`github:`, `google:`, `auth0:`) make those three
    different users. The same goes for nested prefixes such as `git` and
    `github:`: one is a prefix of the other, so one issuer's names can be
    spelled as the other's.

    `scripts/dev-oidc-k0s.sh` refuses empty, equal and nested prefixes once
    more than one issuer is enabled, and always refuses anything in the
    reserved `system:` namespace. This section is about *this* case —
    several issuers on one cluster. With Dex alone (no Google/Auth0), an
    empty prefix is allowed: every Dex connector resolves to one identity
    namespace, so there is no second issuer to collide with, and some
    downstream consumers (a username-keyed record with no prefix scheme)
    need the bare username to recognise the identity at all.

### The configuration

```yaml
apiVersion: apiserver.config.k8s.io/v1
kind: AuthenticationConfiguration
jwt:
  # GitHub, through Dex.
  - issuer:
      url: https://dex.example.com
      audiences: ["kubernetes"]
    claimMappings:
      username: { claim: preferred_username, prefix: "github:" }
      groups: { claim: groups, prefix: "github:" }

  # Google, direct. The audience is the OAuth client ID.
  - issuer:
      url: https://accounts.google.com
      audiences: ["<id>.apps.googleusercontent.com"]
    claimValidationRules:
      - expression: "claims.?email_verified.orValue(false) == true"
        message: "the Google account's email address is not verified"
    claimMappings:
      username: { claim: email, prefix: "google:" }
      # No groups: Google issues no groups claim.

  # Auth0, direct. The URL is exactly what discovery reports, slash included.
  - issuer:
      url: https://dev-a1b2c3d4.us.auth0.com/
      audiences: ["<auth0-client-id>"]
    claimValidationRules:
      - expression: "claims.?email_verified.orValue(false) == true"
        message: "the Auth0 user's email address is not verified"
    claimMappings:
      username: { claim: email, prefix: "auth0:" }
      # Only with the post-login Action from "Groups, if you want them":
      # groups: { claim: "https://banlieue.io/groups", prefix: "auth0:" }
```

What each part is for:

- **`audiences`** is the OAuth client ID. Google and Auth0 both put it in
  the ID token's `aud`, so a token minted for some other application at the
  same provider is rejected even though its signature is valid.
- **`claimValidationRules`** require `email_verified` to be present *and*
  true. `claim: email` on its own applies
  `claims.?email_verified.orValue(true) == true`, which accepts a token that
  omits the claim. For an issuer that lets anyone sign up with any address
  (an Auth0 database connection does, until the address is verified), the
  stricter rule is what makes the email an identity rather than a claim
  somebody typed.
- **No `certificateAuthority`** on Google or Auth0: both chain to public
  roots, so the API server uses the host's trust store. Dex needs one only
  when it serves a private certificate.
- **`url` must be unique** across entries and must not equal the cluster's
  `--service-account-issuer`.

### Registering the clients for the direct topology

Google and Auth0 talk to `kubelogin`, not to Dex, so they redirect to
`kubelogin`'s local listener. Keep any existing Dex registrations: a client
can list both sets of redirect URIs, or you can create a second client.

| Provider | Client | Redirect URIs | Secret in the kubeconfig? |
| --- | --- | --- | --- |
| Google | *Web application* ([step 2](#2-create-the-client)) | `http://localhost:8000`, `http://localhost:18000` | Yes. Google requires a client secret at its token endpoint, so `kubelogin` is given it and it lands in the kubeconfig. Treat that kubeconfig as sensitive, and keep it out of anything shared |
| Auth0 | *Native* ([3b](#3b-direct-to-the-api-server_1)) | `http://localhost:8000`, `http://localhost:18000` | No. Public client; `kubelogin` uses PKCE |

Before configuring Auth0, read its issuer back from discovery (see [the
trailing slash](#2-the-trailing-slash)): the configured `url` has to match
`iss` byte for byte.

### Applying it on k0s

`scripts/dev-oidc-k0s.sh` writes the file to
`/etc/k0s/oidc/authentication-config.yaml` on every controller. Google and
Auth0 are added when their variables are set; Dex stays as it was.

```sh
export KUBECONFIG=~/.kube/my-cluster.yaml
export ISSUER=https://dex.example.com EXPOSE=tailscale CLIENT_ID=kubernetes
export BANLIEUE_GOOGLE_CLIENT_ID=<id>.apps.googleusercontent.com
export BANLIEUE_AUTH0_ISSUER=https://dev-a1b2c3d4.us.auth0.com/
export BANLIEUE_AUTH0_CLIENT_ID=<auth0-client-id>

./scripts/dev-oidc-k0s.sh config    # print the file, touch nothing
./scripts/dev-oidc-k0s.sh attach    # write it on every controller
```

`attach`, in order:

1. Refuses overlapping or empty prefixes, and an Auth0 issuer that differs
   from what Auth0's discovery reports.
2. From **every controller**, fetches each issuer's discovery document, so
   an unreachable issuer is found before any API server is touched.
3. Writes the file aside and renames it into place (the API server watches
   it, and a half-written file is a failed reload), keeping the previous one
   as `authentication-config.yaml.prev`.
4. Waits until that controller's API server reports it is **running this
   file**: the `hash` label of
   `apiserver_authentication_config_controller_last_config_info` is the
   sha256 of the file's bytes. A file the API server refuses keeps the old
   hash, and the script puts the previous file back. That matters because
   a refused file is harmless only until the next restart, when it stops
   kube-apiserver from starting.

Only the first enablement (when `/etc/k0s/k0s.yaml` does not yet name the
file) restarts `k0scontroller`, one controller at a time.

To check it by hand on any controller:

```sh
sudo sha256sum /etc/k0s/oidc/authentication-config.yaml
kubectl --server https://<controller>:6443 get --raw /metrics \
  | grep -E '^apiserver_authentication_config_controller_(last_config_info|automatic_reloads_total)'
```

The two hashes agree, and `automatic_reloads_total{status="failure"}` is
absent or unchanged.

### Logging in with each

One kubectl context per issuer. `PROVIDER` picks which:

```sh
./scripts/dev-oidc-k0s.sh login                     # context banlieue-oidc         -> github:octocat
export BANLIEUE_GOOGLE_CLIENT_SECRET=<secret>
PROVIDER=google ./scripts/dev-oidc-k0s.sh login     # context banlieue-oidc-google  -> google:you@example.com
PROVIDER=auth0  ./scripts/dev-oidc-k0s.sh login     # context banlieue-oidc-auth0   -> auth0:you@example.com
./scripts/dev-oidc-k0s.sh status                    # per-controller hashes, issuers, who you are in each
```

`PROVIDER=... ./scripts/dev-oidc-k0s.sh kubeconfig` prints the same thing as
a standalone kubeconfig for another machine.

A fresh login has no access beyond the Kubernetes defaults: nothing is bound
to `system:authenticated`, because any account at any of the three providers
can finish a login. Bind the identities you mean:

```sh
PROVIDER=google ./scripts/dev-oidc-k0s.sh grant    # RoleBinding banlieue-claim-author-oidc-google
```

### Claims from every issuer

banlieue's claim subject policy checks that a claim names its own creator:
the prefix plus `spec.subject.id` must equal the authenticated username.
With several issuers, give the policy one prefix per issuer in its
ConfigMap's `issuerPrefixes`, matching the AuthenticationConfiguration, and
list each issuer in `issuers`:

```yaml
# ConfigMap banlieue-system/banlieue-claim-subject-policy
data:
  issuers: |
    https://dex.example.com
    https://accounts.google.com
    https://dev-a1b2c3d4.us.auth0.com/
  usernamePrefix: |
    github:
  issuerPrefixes: |
    https://dex.example.com github:
    https://accounts.google.com google:
    https://dev-a1b2c3d4.us.auth0.com/ auth0:
```

A Google user then creates a claim with `subject.issuer:
https://accounts.google.com` and `subject.id: you@example.com`. Because each
issuer's prefix is distinct, the policy also refuses a claim whose issuer is
not the one that authenticated its author: a GitHub user cannot record a
sandbox as Google's. An issuer with no `issuerPrefixes` line falls back to
`usernamePrefix` (ADR-0047 Decision 9, amended 2026-10-13).

`grant` keeps this in step: for `PROVIDER=google` or `auth0` it adds that
issuer to `issuers` and its line to `issuerPrefixes`, leaving the others as
they are.

---

## Wiring it up

Once the client exists, the cluster side is in [Testing claim
authorization](../guides/testing-claim-authorization.md):

```sh
export GITHUB_CLIENT_ID=... GITHUB_CLIENT_SECRET=...
make dev-oidc-up                                 # a new cluster, OIDC baked in
# or, keeping an existing cluster's pools and VMs:
CLUSTER=banlieue-demo make dev-oidc-attach
CLUSTER=banlieue-demo make dev-oidc-login
```

On an existing k0s cluster, `scripts/dev-oidc-k0s.sh` does the same with
Dex, and adds Google and Auth0 as direct issuers beside it: [All three on
one cluster](#all-three-on-one-cluster).

`scripts/dev-oidc-kind.sh` ships the **GitHub** connector. For Google or
Auth0, edit the `connectors:` block in its `deploy_dex` function with the YAML
above — the rest of the setup (certificates, API server flags, RBAC, the
policy's issuer allowlist) is provider-independent.

## Troubleshooting

| Symptom | Cause |
| --- | --- |
| `redirect_uri_mismatch` | You registered the wrong component's callback. Dex setups register Dex's `/dex/callback`; direct setups register `kubelogin`'s `localhost:8000` and `:18000`. |
| API server rejects every token, issuer mismatch | `--oidc-issuer-url` is not byte-identical to the token's `iss`. Fetch discovery and use the `issuer` it reports, trailing slash included. |
| GitHub: a user in the org appears not to be | Private org membership. Make it public or have the org approve the OAuth App. |
| Google: "you do not have access" | External + Testing app with the account missing from the test-user list. |
| Google: logins break exactly a week later | An unpublished External app re-prompts consent every 7 days. |
| Google: Dex refuses the login over `email_verified` | Common on Workspace. `insecureSkipEmailVerified: true`, understanding what it drops. |
| Auth0: every token rejected as an issuer mismatch | The trailing slash. `--oidc-issuer-url` must be exactly what discovery reports, which for Auth0 ends in `/`. |
| Auth0: a custom claim never appears in the token | It is not namespaced. Auth0 drops non-URI custom claims without an error. |
| Auth0: the Action exists but changes nothing | It was built but never added to **Actions → Flows → Login**. |
| One issuer works, a newly added one is ignored | The API server refused the new file and kept running the old one. Compare `sha256sum` of the file with the `hash` label of `apiserver_authentication_config_controller_last_config_info`; the API server log names the field it rejected. |
| Google or Auth0 login succeeds, API server answers 401 | `aud` is not in that entry's `audiences` (it must be the client ID), or `email_verified` is false or missing. Decode the ID token from `~/.kube/cache/oidc-login` and compare. |
| Auth0 database user rejected while a social login works | The address was never verified. The `email_verified` rule refuses it by design; verify the address, or use the social connection. |
| A Google/Auth0 identity is refused when creating a claim naming itself | Its issuer has no line in the policy's `issuerPrefixes`, so `usernamePrefix` (Dex's) applies, or the issuer is missing from `issuers`. See [Claims from every issuer](#claims-from-every-issuer). |
| `kubectl auth whoami` shows a long opaque string | `--oidc-username-claim=sub`. Use `email` or `preferred_username`; `sub` cannot be typed into `spec.subject.id`. |

## Reference

- [Testing claim authorization](../guides/testing-claim-authorization.md) — the cluster side
- [VirtualMachine Claims](../guides/virtualmachine-claims.md) — what `spec.subject` is for
- [Dex connectors](https://dexidp.io/docs/connectors/): [GitHub](https://dexidp.io/docs/connectors/github/), [Google](https://dexidp.io/docs/connectors/google/), [OIDC (Auth0)](https://dexidp.io/docs/connectors/oidc/)
- [Auth0: OIDC discovery](https://auth0.com/docs/get-started/applications/configure-applications-with-oidc-discovery)
- [Kubernetes OIDC authentication](https://kubernetes.io/docs/reference/access-authn-authz/authentication/#openid-connect-tokens)
- [kubelogin](https://github.com/int128/kubelogin)
