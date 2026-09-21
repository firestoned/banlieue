# How a Claim Works: the whole path, in diagrams

This page describes *how the claim path is wired*: *who talks to whom*, in
order, from a developer typing `kubectl` to a sandbox VM destroyed by its own
deadline. For the task-level view — which fields to set, and what each one
means — read the [VirtualMachine Claims](../guides/virtualmachine-claims.md)
guide instead; the two are companions.

Every diagram here is drawn from the shipped code, not from intent. Where
something is designed but not built, it says so in the diagram itself.

## The cast

Claims involve more actors than most banlieue resources, because identity is
part of the design: a claim records *who* a sandbox was handed to, so the
thing that establishes "who" is inside the picture.

| Actor | What it is | Where it lives |
| --- | --- | --- |
| **Developer** | A human who wants a sandbox VM | a laptop |
| **kubectl** | The CLI | the same laptop |
| **kubelogin** | `kubectl oidc-login`, an exec credential plugin. Runs the browser flow, caches the ID token | the same laptop |
| **Browser** | Where the OAuth2 consent happens | the same laptop |
| **GitHub** | The identity provider. **OAuth2, not OIDC** — it mints no ID tokens | the internet |
| **Dex** | The OIDC bridge in front of GitHub. Mints the ID token the API server will accept | in-cluster |
| **API server** | Authenticates the token, then runs admission | control plane |
| **Admission policy** | `ValidatingAdmissionPolicy` — checks `spec.subject` against the caller (ADR-0047 D10) | control plane |
| **Claim controller** | Binds a member, holds it, releases it | `banlieue-controller` |
| **Pool controller** | Keeps `warmReplicas` Ready members | `banlieue-controller` |
| **Provider controller** | Creates and destroys the real VM | `banlieue-provider-libvirt`, `-vsphere` |
| **Hypervisor** | libvirtd / vCenter | a host somewhere |
| **Broker** | Hands sandboxes out on behalf of other people (roadmap phase C) | *not built* |
| **In-guest agent** | Receives the subject's credential and verifies it (ADR-0049) | *not built, separate repo* |

!!! note "Dex is a development convenience, not a requirement"
    Any OIDC issuer the API server trusts works the same way — Google, Auth0,
    your corporate IdP. Dex appears here because **GitHub is OAuth2 and has
    no ID tokens**, so it cannot be an API server issuer on its own. See
    [OAuth client setup](../developer/oauth-clients.md) and
    [Testing claim authorization](../guides/testing-claim-authorization.md).

## 1. Identity: how a developer becomes a username

Nothing about a claim makes sense until this has happened. The API server
can only vouch for `request.userInfo`, and this is how that gets populated.

```mermaid
sequenceDiagram
    autonumber
    actor Dev as Developer
    participant K as kubectl
    participant KL as kubelogin
    participant B as Browser
    participant GH as GitHub, OAuth2 only
    participant Dex as Dex, the OIDC bridge
    participant API as API server

    Dev->>K: kubectl get vmclaim
    K->>KL: exec credential plugin
    Note over KL: kubeconfig carries issuer URL,<br/>client id and secret, plus the<br/>extra scopes profile, email, groups

    alt cached token still valid by exp
        KL-->>K: cached ID token
    else no usable cached token
        KL->>B: open the authorize URL, with PKCE
        B->>Dex: GET /auth
        Dex->>GH: OAuth2 authorization code flow
        GH->>B: consent screen
        Dev->>B: approve
        B->>GH: grant
        GH-->>Dex: access token + user profile
        Note over Dex: Dex is the bridge: it turns an<br/>OAuth2 profile into a signed ID token
        Dex-->>B: redirect with code
        B-->>KL: code, via the localhost callback
        KL->>Dex: POST /token with the code and PKCE verifier
        Dex-->>KL: signed ID token, a JWT
        KL->>KL: cache under ~/.kube/cache/oidc-login
        KL-->>K: ID token
    end

    K->>API: request + Authorization: Bearer <ID token>
    API->>Dex: fetch JWKS once, then cache it
    API->>API: verify signature, iss, aud, exp
    API->>API: username = oidc-username-prefix + the configured claim
    Note over API: a sub of octocat becomes<br/>a userInfo.username of oidc:octocat
    API-->>K: response
```

Two failure modes worth recognising, both learned the hard way:

!!! warning "kubelogin decides freshness from `exp` alone"
    If the issuer rotates its signing keys, the cached token is still
    *unexpired* and kubelogin keeps serving it — while the API server
    rejects it, forever. No amount of re-running the login escapes it. The
    only cure is `rm -rf ~/.kube/cache/oidc-login`. Dex with
    `storage: type: memory` rotates keys on every restart, so this is not
    hypothetical.

!!! warning "`iss` must match byte-for-byte across three consumers"
    The browser, kubectl and the API server must all reach the issuer at the
    *same* URL, because `iss` inside the token is compared literally against
    `--oidc-issuer-url`. In kind this forces a NodePort plus a host-port
    mapping, and therefore an **IP SAN** rather than a DNS name.

### Where the prefix goes, and where it does not

This distinction is the reason ADR-0047 Decision 9 was amended:

```mermaid
flowchart LR
    subgraph JWT["ID token, as the issuer minted it"]
        S["sub / preferred_username<br/><b>octocat</b>"]
    end
    subgraph K8S["Kubernetes, after --oidc-username-prefix"]
        U["userInfo.username<br/><b>oidc:octocat</b>"]
    end
    subgraph CR["VirtualMachineClaim"]
        C["spec.subject.id<br/><b>octocat</b>"]
    end

    S -->|"API server prepends the prefix"| U
    S ==>|"stored raw — this is the value<br/>a JWT actually carries"| C
    U -.->|"policy re-applies the prefix<br/>only to compare"| C

    style C fill:#e8f5e9,stroke:#2e7d32
    style U fill:#fff3e0,stroke:#ef6c00
```

`spec.subject.id` stores the **raw** subject. The in-guest agent compares it
against a JWT, and no token anywhere carries an `oidc:` prefix — so storing
the prefixed Kubernetes username would make the field uncomparable to the
only thing it exists to be compared against. The admission policy adds the
prefix at check time instead, from `usernamePrefix` in its params ConfigMap.

## 2. Creating a claim: what admission checks

`spec.subject` is a free-text assertion by whoever created the claim, and
without this policy anyone with `create virtualmachineclaims` can attribute
a VM to any identity they like — with the audit log recording it as genuine.
The check has to happen at admission, while the requesting principal still
exists; by reconcile time the controller sees an object, never a caller.

```mermaid
sequenceDiagram
    autonumber
    actor Dev as Developer
    participant K as kubectl
    participant API as API server
    participant VAP as ValidatingAdmissionPolicy
    participant CM as ConfigMap<br/>banlieue-claim-subject-policy
    participant ETCD as etcd

    Dev->>K: kubectl create -f claim.yaml
    K->>API: CREATE virtualmachineclaims
    API->>API: authenticate → userInfo.username
    API->>API: authorize (RBAC)
    API->>VAP: admission review
    VAP->>CM: paramRef, with parameterNotFoundAction Deny
    CM-->>VAP: issuers, brokers, usernamePrefix

    Note over VAP: variables: split on newlines,<br/>drop blanks, trim

    alt caller is in brokers
        VAP->>VAP: skip the id check — handing<br/>sandboxes to others is its job
    else ordinary caller
        VAP->>VAP: usernamePrefix + subject.id == userInfo.username ?
    end
    VAP->>VAP: subject.issuer in issuers ?

    alt all validations pass
        VAP-->>API: allow
        API->>ETCD: persist
        API-->>K: created
        K-->>Dev: virtualmachineclaim/foo created
    else any validation fails
        VAP-->>API: deny, reason Forbidden
        API-->>K: 403 + the policy's messageExpression
        K-->>Dev: error explaining which field and why
    end
```

### The five validations, and why each is scoped as it is

```mermaid
flowchart TD
    OP{"request.operation"}
    OP -->|CREATE| C1["1. subject.id binds to the caller<br/>unless the caller is a broker"]
    C1 --> C2["2. subject.issuer is in the allowlist"]
    C2 --> OK1([allow])

    OP -->|UPDATE| U3["3. spec.subject is immutable"]
    U3 --> U4["4. spec.poolRef is immutable"]
    U4 --> U5["5. spec.ttlSeconds is immutable"]
    U5 --> OK2([allow])

    style C1 fill:#e3f2fd,stroke:#1565c0
    style C2 fill:#e3f2fd,stroke:#1565c0
    style U3 fill:#fce4ec,stroke:#ad1457
    style U4 fill:#f3e5f5,stroke:#6a1b9a
    style U5 fill:#f3e5f5,stroke:#6a1b9a
```

!!! danger "Checks 1 and 2 are CREATE-only, and that is load-bearing"
    On UPDATE the requester is whoever is touching the object *now* — which
    is normally **the claim controller adding its finalizer**, not the person
    the claim is for. Applying the identity check to updates denies the
    controller, so claims are created and then never reconciled: `Bound` is
    never reached and no VM is ever bound. The visible symptom is
    indistinguishable from "banlieue is not running".

    Nothing is lost by the scoping, because check 3 freezes `subject`: an
    attribution authorized at creation cannot be edited afterwards by
    anyone, controller included. Checks 4 and 5 exist for a plainer reason —
    mutating those fields would silently do nothing, which is worse than
    being refused.

## 3. Binding: how a claim gets a VM

The claim controller never locks anything. The API server is already the
serialisation point, so a `resourceVersion` precondition on the member patch
is the entire concurrency story.

```mermaid
sequenceDiagram
    autonumber
    participant API as API server
    participant CC as Claim controller
    participant PC as Pool controller
    participant PROV as Provider controller
    participant HV as Hypervisor

    API-->>CC: watch event: claim created
    CC->>API: ensure finalizer banlieue.io/claim-protection

    Note over CC: next_step sees not deleting,<br/>not expired, not bound, so Wait,<br/>and now it looks for a member

    CC->>API: GET pool named by spec.poolRef
    CC->>API: LIST vms labelled banlieue.io/pool=<pool>
    CC->>CC: pick_member: Ready only, current image<br/>revision first, then Ready longest, then name

    alt a Ready member exists
        CC->>API: PATCH member, merge patch carrying a resourceVersion precondition
        Note over CC,API: label banlieue.io/claim=<claim><br/>annotate claim-subject-{issuer,id}<br/>ownerReferences → the claim

        alt precondition holds
            API-->>CC: 200
            CC->>API: PATCH claim status
            Note over CC,API: phase Bound, virtualMachineRef,<br/>a 128-bit random nonce, boundAt,<br/>expiresAt = boundAt + ttlSeconds
            API-->>PC: watch event: member is now Claimed
            PC->>PC: Claimed members are invisible to sizing —<br/>they only count against maxReplicas
            PC->>API: CREATE replacement member
            PROV->>HV: provision the replacement
        else somebody else wrote the member first
            API-->>CC: 409 Conflict
            CC->>CC: lost the bind race, so requeue in 1s and pick again
        end
    else nothing Ready
        CC->>API: PATCH claim status: phase Pending + why
        Note over CC: requeue in 3s — the shortest in the<br/>controller, because a human is blocked on it
    end
```

Three consequences of that shape:

- **Re-parenting is what makes a claim safe.** The member's owner becomes the
  claim, not the pool, so deleting the pool cannot destroy a sandbox somebody
  is using (ADR-0047 D3).
- **The subject is stamped onto the VM** as
  `banlieue.io/claim-subject-issuer` / `-id` annotations. That is the whole
  of banlieue's interest in `subject`: it records the attribution and reads
  it no further.
- **The nonce is minted at bind**, is single-use, and is *not secret*. Its
  only job is to stop an attestation quote captured from one sandbox being
  replayed against another.

### Two consumers, one member

```mermaid
sequenceDiagram
    autonumber
    participant CA as claim-a reconcile
    participant API as API server
    participant CB as claim-b reconcile

    CA->>API: LIST members
    CB->>API: LIST members
    API-->>CA: vm-7 is Ready (resourceVersion 100)
    API-->>CB: vm-7 is Ready (resourceVersion 100)

    Note over CA,CB: both picked the same member from<br/>the same snapshot

    CA->>API: PATCH vm-7 if resourceVersion == 100
    API-->>CA: 200 — now at 101
    CB->>API: PATCH vm-7 if resourceVersion == 100
    API-->>CB: 409 Conflict
    CB->>CB: requeue 1s, re-list, pick a different member
```

No lease, no lock, no leader coordination for the hand-out itself. A
`Claimed` member is never offered by `pick_member` again, which is the
invariant the whole design rests on.

## 4. The claim state machine

```mermaid
stateDiagram-v2
    direction LR
    [*] --> Pending : created

    Pending --> Bound : a Ready member was taken
    Pending --> Pending : nothing Ready — retry every 3s,<br/>unboundedly on purpose
    Pending --> Releasing : deleted before it ever bound

    Bound --> Bound : hold — mirror the member's addresses
    Bound --> Releasing : TTL reached, or deleted
    Bound --> Failed : the bound member disappeared

    Releasing --> [*] : member gone, finalizer removed

    Failed --> Releasing : deleted
    Failed --> [*] : terminal otherwise

    note right of Pending
        Pending never times out.
        A pool may simply be filling,
        and a deadline here would
        fail claims that were about
        to succeed. (ADR-0047 D11)
    end note

    note right of Failed
        Terminal, never rebound: a
        consumer holding this claim
        believes it is talking to one
        specific VM. (ADR-0047 D7)
    end note
```

### Precedence is the interesting part

`next_step` is a pure function of a snapshot, and the order it tests things
in is a design decision rather than an implementation detail:

```mermaid
flowchart TD
    START(["one reconcile pass"]) --> D1{"deletionTimestamp set<br/>OR expiresAt passed?"}
    D1 -->|yes| REL["<b>Release</b><br/>destroy the member, then let go"]
    D1 -->|no| D2{"status.virtualMachineRef set?"}
    D2 -->|yes| D3{"does that member still exist?"}
    D3 -->|no| FAIL["<b>Fail</b><br/>terminal — never a rebind"]
    D3 -->|yes| HOLD["<b>Hold</b><br/>mirror addresses, sleep"]
    D2 -->|no| D4{"pick_member found a candidate?"}
    D4 -->|yes| BIND["<b>Bind</b><br/>take it"]
    D4 -->|no| WAIT["<b>Wait</b><br/>phase Pending, retry in 3s"]

    style REL fill:#ffebee,stroke:#c62828
    style FAIL fill:#fce4ec,stroke:#ad1457
    style HOLD fill:#e8f5e9,stroke:#2e7d32
    style BIND fill:#e3f2fd,stroke:#1565c0
    style WAIT fill:#fff3e0,stroke:#ef6c00
```

Release outranks everything, **including a healthy binding**: once a claim is
going away, or its deadline has passed, the only remaining job is to destroy
the member. And a vanished member fails the claim even when a replacement is
sitting Ready, because rebinding would hand the consumer a different VM than
the one it believes it holds.

## 5. Release: the deadline, and the two finalizers

A claim's TTL is a **deadline**, not an idle timer. It is counted from the
bind instant — so a claim that waited ten minutes for capacity still gets its
full TTL — and nothing extends it.

```mermaid
sequenceDiagram
    autonumber
    actor Dev as Developer
    participant API as API server
    participant CC as Claim controller
    participant PROV as Provider controller
    participant HV as Hypervisor
    participant PC as Pool controller

    alt the developer is done early
        Dev->>API: kubectl delete vmclaim foo
        API->>API: set deletionTimestamp —<br/>the claim finalizer blocks actual removal
    else the TTL simply runs out
        Note over CC: is_expired(status.expiresAt, now)
    end

    API-->>CC: watch / requeue
    CC->>API: DELETE member, background propagation
    CC->>API: PATCH claim status: phase Releasing<br/>reason Released or Expired

    API->>API: member gets deletionTimestamp —<br/>the provider's finalizer blocks removal
    API-->>PROV: watch event: member deleting
    PROV->>HV: destroy the domain / VM, free volumes
    HV-->>PROV: gone
    PROV->>API: remove the provider finalizer
    API->>API: member object actually disappears

    API-->>CC: requeue: member is 404 now
    alt the developer deleted the claim
        CC->>API: remove banlieue.io/claim-protection
        Note over API: claim object disappears
    else it expired on its own
        CC->>API: DELETE the claim itself
        Note over CC: an expired claim must not look like<br/>a live hold on a VM that is gone
    end

    API-->>PC: the pool is short a member
    PC->>API: CREATE a replacement
```

!!! tip "\"Claim gone\" means \"sandbox gone\", transitively"
    The ordering is the guarantee. The claim keeps its finalizer until the
    member is gone from the API server, and the member's own finalizer keeps
    *it* until the backend VM is actually destroyed. Neither layer trusts
    the other to have finished — each one waits and re-checks.

## 6. Delivering the subject's credential

!!! danger "Designed, not built — ADR-0049 is Proposed"
    Nothing in this section ships today. It is here because it is the reason
    `status.nonce` and `status.tpmEndorsementCertificates` exist, and because
    "how does the JWT get into the VM?" is the first question the claim model
    provokes. ADR-0049 also depends on ADR-0045 (EK certificates), which is
    itself not landed.

A claim deliberately carries no credential: a custom resource is readable by
every reader of the namespace and lands in the audit log, so a token in it is
a token disclosed. And a warm pool member **exists before any claim does**,
so nothing baked in at build or boot can be subject-specific. The credential
therefore has to arrive later, over a channel banlieue is not part of.

```mermaid
sequenceDiagram
    autonumber
    actor Dev as Developer
    participant BR as Broker<br/>not built
    participant API as API server
    participant AG as In-guest agent<br/>not built, own repo
    participant TPM as per-VM vTPM
    participant IDP as Issuer JWKS

    Dev->>BR: asks for a sandbox, presenting a token
    BR->>API: CREATE claim, setting subject on their behalf
    API-->>BR: claim bound, carrying virtualMachineRef,<br/>addresses, nonce, tpmEndorsementCertificates

    BR->>AG: connect over mTLS, send {claim, nonce, JWT}
    AG->>TPM: quote over status.nonce
    TPM-->>AG: signed quote
    AG-->>BR: quote
    BR->>BR: verify quote against the EK certificate<br/>published on the claim (ADR-0045)

    alt quote verifies
        Note over AG: the agent now holds the JWT and checks<br/>iss == subject.issuer, the subject claim<br/>== subject.id, aud == its OWN configured<br/>audience, plus exp, nbf and the signature
        AG->>IDP: fetch JWKS to verify the signature
        IDP-->>AG: keys
        AG->>AG: start the workload
    else quote fails
        BR->>AG: nothing
        Note over AG,TPM: no token, no workload. The claim's TTL<br/>reaps the VM as usual — there is no<br/>partial-trust mode
    end
```

Three points that are decisions rather than mechanics:

- **banlieue neither carries nor validates the token.** It records the
  attribution, publishes the nonce, and mirrors the EK certificate.
  Verification belongs to the agent, the only party that can legitimately
  hold the credential — which is what keeps subject credentials out of the
  controller's compromise radius.
- **`aud` must be the agent's own configured audience, never read from the
  claim.** Taking it from the claim is the obvious implementation, and it
  silently removes the guarantee: whoever wrote the claim would choose the
  audience, so a token minted for any other service would satisfy the check.
- **The issuer allowlist turns out to be load-bearing twice.** The agent
  learns *which* issuer to trust from `subject.issuer` — a field on a CR — so
  without the admission allowlist, anyone who could create a claim would
  point the agent at their own JWKS and every token they minted would
  validate. It was added for audit honesty; it is also what makes signature
  verification meaningful.

!!! note "`GuestReady` is a different signal, and must not be conflated"
    A guest that announced itself has booted from its installed disk
    (ADR-0043). Only a verified quote says *which* guest it is.

## 7. Everything at once

```mermaid
flowchart TB
    subgraph LAPTOP["Developer's laptop"]
        DEV(["Developer"])
        KUBECTL["kubectl"]
        KLOGIN["kubelogin<br/>token cache"]
        BROWSER["Browser"]
    end

    subgraph EXTERNAL["Outside the cluster"]
        GITHUB["GitHub<br/>OAuth2"]
    end

    subgraph CLUSTER["Kubernetes cluster"]
        DEX["Dex<br/>OIDC bridge"]
        subgraph CP["Control plane"]
            APISERVER["API server<br/>authn + authz"]
            POLICY["Admission policy<br/>subject authorization"]
        end
        subgraph BANLIEUE["banlieue-system"]
            CLAIMC["Claim controller"]
            POOLC["Pool controller"]
            PROVC["Provider controller"]
        end
        subgraph OBJ["Objects"]
            POOL["VirtualMachinePool"]
            CLAIM["VirtualMachineClaim"]
            VM["VirtualMachine<br/>+ infra CR"]
        end
    end

    subgraph HOST["Hypervisor host"]
        LIBVIRT["libvirtd / vCenter"]
        SANDBOX["Sandbox VM<br/>+ per-VM vTPM"]
    end

    DEV --> KUBECTL
    KUBECTL <--> KLOGIN
    KLOGIN <--> BROWSER
    BROWSER <--> DEX
    DEX <--> GITHUB
    KUBECTL -->|"Bearer ID token"| APISERVER
    APISERVER -->|"verify signature via JWKS"| DEX
    APISERVER --> POLICY
    POLICY -->|"allow / deny"| APISERVER

    APISERVER --- CLAIM
    APISERVER --- POOL
    APISERVER --- VM

    POOLC -->|"keeps warmReplicas Ready"| POOL
    POOL -->|"owns, until claimed"| VM
    CLAIMC -->|"binds, re-parents, expires"| CLAIM
    CLAIM -.->|"ownerRef after bind"| VM
    PROVC --> VM
    PROVC --> LIBVIRT
    LIBVIRT --> SANDBOX

    style POLICY fill:#fff3e0,stroke:#ef6c00
    style CLAIM fill:#e3f2fd,stroke:#1565c0
    style SANDBOX fill:#e8f5e9,stroke:#2e7d32
    style GITHUB fill:#f5f5f5,stroke:#616161
```

The trust boundaries worth naming on that picture:

1. **Laptop → cluster.** Crossed by a bearer token. The API server is the
   only thing that decides what the developer is called.
2. **API server → admission policy.** The last point at which the *caller*
   still exists. Everything after this sees objects.
3. **Controller → hypervisor.** Crossed by provider credentials, never by
   consumer credentials.
4. **Cluster → sandbox guest.** Crossed only by the broker's attested
   channel, in a design that is not built yet. banlieue itself never reaches
   into the guest to deliver anything.

## Where to go next

- [VirtualMachine Claims](../guides/virtualmachine-claims.md) — the field-by-field guide
- [VirtualMachine Pools](../guides/virtualmachine-pools.md) — where members come from
- [Testing claim authorization](../guides/testing-claim-authorization.md) — run the
  whole OIDC flow against a kind cluster with your own GitHub account
- [OAuth client setup](../developer/oauth-clients.md) — GitHub, Google, Auth0
- ADR-0047 (claims), ADR-0049 (attestation), ADR-0043 (`GuestReady`),
  ADR-0045 (EK certificates)
