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
| GitHub (via Dex) | `preferred_username` | `oidc:octocat` — the GitHub login |
| Google | `email` | `oidc:you@example.com` |
| Auth0 | `email` | `oidc:you@example.com` — Auth0 issues no `preferred_username` |

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
--oidc-username-prefix=oidc:
```

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
--oidc-username-prefix=oidc:
```

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
| `kubectl auth whoami` shows a long opaque string | `--oidc-username-claim=sub`. Use `email` or `preferred_username`; `sub` cannot be typed into `spec.subject.id`. |

## Reference

- [Testing claim authorization](../guides/testing-claim-authorization.md) — the cluster side
- [VirtualMachine Claims](../guides/virtualmachine-claims.md) — what `spec.subject` is for
- [Dex connectors](https://dexidp.io/docs/connectors/): [GitHub](https://dexidp.io/docs/connectors/github/), [Google](https://dexidp.io/docs/connectors/google/), [OIDC (Auth0)](https://dexidp.io/docs/connectors/oidc/)
- [Auth0: OIDC discovery](https://auth0.com/docs/get-started/applications/configure-applications-with-oidc-discovery)
- [Kubernetes OIDC authentication](https://kubernetes.io/docs/reference/access-authn-authz/authentication/#openid-connect-tokens)
- [kubelogin](https://github.com/int128/kubelogin)
