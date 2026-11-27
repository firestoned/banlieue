<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0174: The EC2 InfraMachine and a first-party AWS client

- **Status:** Proposed
- **Date:** 2026-11-26
- **Deciders:** Erick Bourgeois
- **Related:** Roadmap 24 (EC2 provider), which reserved ADR-0166 for this
  decision; that number went to
  [ADR-0166](0166-fleet-wide-unique-sandbox-addresses.md), so this ADR takes
  the next free one. Mirrors [ADR-0164](0164-azure-inframachine-and-a-first-party-arm-client.md)
  (the Azure InfraMachine and ARM client). Follows
  [ADR-0011](0011-libvirt-provider-own-client.md) (banlieue writes its own
  backend clients), [ADR-0133](0133-shared-provider-serves-tenant-namespaces.md)
  (shared-capable from the first commit) and
  [ADR-0170](0170-post-quantum-tls-with-aws-lc-rs.md) (aws-lc-rs, post-quantum
  first). Uses [ADR-0158](0158-shape-of-a-cloud-provider-class.md) to
  [ADR-0163](0163-guestready-on-a-cloud.md) and
  [ADR-0173](0173-an-allocation-failure-re-places-the-machine-in-another-zone.md).
  Companions: [ADR-0167](0167-nitrotpm-and-the-ek-public-key.md) (NitroTPM)
  and [ADR-0175](0175-ec2-guest-layer.md) (the guest layer).

## Context

Roadmap 24 adds EC2 instances as a backend. As for Azure, two choices come
before code: what the InfraMachine kind is called, and how banlieue talks to
AWS.

The phase 0 spike (2026-11-24 to 2026-11-26, one account, `ca-central-1`)
drove EC2, STS, S3 and VM Import from scripts and recorded every request and
failure. What a client must handle, from that:

1. **Create is one idempotent call.** `RunInstances` with a `ClientToken`
   returns the same instance on a retry; the same token with different
   parameters fails with `IdempotentParameterMismatch` (confirmed both ways).
   `TagSpecifications` tag the instance and its volumes in that same call.
2. **Every AWS API banlieue needs negotiates X25519MLKEM768** (STS, EC2, S3,
   EBS) from rustls with aws-lc-rs, the provider ADR-0170 installed.
3. **The protocols are old and mixed.** EC2 and STS speak the Query protocol
   (form-encoded `POST`, XML responses, errors as `<Response><Errors>`), S3
   speaks REST with XML bodies, Service Quotas and the Resource Groups
   Tagging API speak JSON 1.1 (`X-Amz-Target`). Every signed request needs
   SigV4; `AssumeRoleWithWebIdentity` is the one unsigned call.
4. **Workload identity federation works.** A ServiceAccount token exchanged
   by `AssumeRoleWithWebIdentity` for a one-hour session; a token for the
   wrong subject or the wrong audience was refused (ADR-0159).
5. **IAM can bound the provider tightly, by condition keys.** A policy that
   allowed `RunInstances` only with `ec2:MetadataHttpTokens=required`, a hop
   limit of 1, encrypted volumes, no public IP, the ownership tag on the
   request, from the account's own tagged AMIs and in tagged security
   groups passed Access Analyzer with no findings and 60 of 60 simulated
   cases, and a live run allowed every intended call and refused each
   variant it should (IMDSv1, hop 2, untagged, public IP, another type).
6. **The console is the only read-only channel out of a guest.**
   `GetConsoleOutput` with `Latest` returns the serial console, refreshed
   roughly every 30 s; a line written to `/dev/kmsg` at error level reaches
   it even from a `quiet` boot (ADR-0175).
7. **IMDS can be turned off after boot.** `ModifyInstanceMetadataOptions
   --http-endpoint disabled` applied to a running instance at once.
8. **Zones are per-account names over global IDs** (`ca-central-1a` is
   `cac1-az1` in that account). Offered types come from
   `DescribeInstanceTypeOfferings` per zone ID; the vCPU quota from Service
   Quotas (`L-1216C47A`, default 5, raised to 16 in six minutes).
9. **Timing.** From `RunInstances` to the guest's ready line on the console:
   144 to 156 s for the first unsealed images, **413 s** for a raw sealed
   image that reset itself out of recovery on first boot, **49.9 s** for a
   pre-laid, sealed 8 GiB template on a 10 GiB volume (ADR-0175),
   and 40 s for a restart. First boots of three templates of the same design
   spread from 29 to 64 s of guest uptime, attributed to EBS fetching each
   block of a snapshot-restored volume on first read.
10. **Official Rust crates exist for every service** (`aws-sdk-ec2`,
    `aws-sdk-sts`, `aws-sdk-s3`, and the rest), unlike Azure's. They bring
    the smithy HTTP and runtime stack and a dependency tree several times
    banlieue's current one, with releases every week.

## Decision

### 1. Kind `EC2Machine` (and `EC2MachineTemplate`)

In `infrastructure.banlieue.io/v1alpha1`, short name `ec2m`. Cluster API's
AWS provider owns `AWSMachine`; `EC2Machine` cannot be confused with it and
names what it creates. It satisfies the CAPI v1beta2 InfraMachine contract:

- `spec.providerID`: `aws:///<zone-name>/<instance-id>`, the form the AWS
  cloud controller manager and CAPA use;
- `status.addresses`: the primary private IPv4 (`InternalIP`) and the
  instance's private DNS name (`InternalDNS`); no public or Elastic IP in v1;
- `spec.failureDomain`: the zone **name**, with the zone ID recorded in
  status so two accounts can be compared (ADR-0158 Decision 3);
- `status.initialization.provisioned`.

Its `providerRef` is a `ProviderReference`, and the provider serves tenant
namespaces through `ProviderScope` (ADR-0133).

### 2. A first-party crate, `banlieue-aws`

A small typed client, as `banlieue-azure`, `banlieue-proxmox` and
`banlieue-libvirt` are, rather than the official SDK (finding 10). It covers
only what banlieue calls:

- **one `reqwest` client**, rustls with aws-lc-rs, X25519MLKEM768 offered
  first (ADR-0170);
- **SigV4 written in the crate** on aws-lc-rs's HMAC-SHA-256, tested against
  AWS's published SigV4 test suite vectors, with unsigned-payload support
  for S3 part uploads;
- **a credential source from `AssumeRoleWithWebIdentity`** (ADR-0159): the
  projected token re-read on every exchange, the session refreshed five
  minutes before it expires, never logged or written anywhere;
- **typed requests** for EC2 (`RunInstances`, `DescribeInstances`,
  `StopInstances`, `StartInstances`, `TerminateInstances`,
  `GetConsoleOutput`, `GetInstanceTpmEkPub`,
  `ModifyInstanceMetadataOptions`, `DescribeInstanceTypeOfferings`,
  `DescribeInstanceTypes`, `DescribeAvailabilityZones`, `DescribeImages`,
  and for the publisher `ImportSnapshot`, `DescribeImportSnapshotTasks`,
  `RegisterImage`, `DeregisterImage`, `DeleteSnapshot`, `CreateTags`), S3
  multipart upload, Service Quotas `GetServiceQuota`, and the Tagging API's
  `GetResources`;
- **EC2's API version pinned** (`2016-11-15`, the Query API's only version)
  and each JSON target pinned, changed deliberately;
- **XML parsed with `quick-xml`** (serde feature), the one new dependency,
  maintained and already common in the Rust AWS ecosystem;
- **errors typed by AWS's error code** (`InsufficientInstanceCapacity`,
  `IdempotentParameterMismatch`, `RequestLimitExceeded`,
  `InvalidInstanceID.NotFound`, `Unsupported`, `UnauthorizedOperation`), so
  the provider can tell "no capacity in this zone" from "bad request" from
  "throttled" from "the role does not allow it";
- **retries**: throttling (`RequestLimitExceeded`, `Throttling`, HTTP 503)
  retried with capped exponential back-off and full jitter, the same budget
  for every verb; sustained throttling is reported to the caller so the
  provider can set a condition on the `Provider`; nothing else is retried
  inside the client;
- **eventual consistency handled where AWS documents it**: an instance ID
  just returned by `RunInstances` may answer `InvalidInstanceID.NotFound`
  for a few seconds, which the observe path treats as "not visible yet",
  never as "gone".

### 3. The provider's lifecycle

- **Create.** One `RunInstances` with `ClientToken` derived from the
  `EC2Machine`'s UID **and the zone** (finding 1): a retry in the same zone
  returns the same instance, and a re-placement in another zone (ADR-0173)
  is a new request with a new token, since the same token with different
  parameters is refused; `TagSpecifications` for the instance, its volumes
  and its network interface carrying ADR-0158's ownership tags; the `Name` tag set to the
  machine's name. Before any create, look up by `banlieue-owner-uid` and
  adopt what exists (ADR-0158 Decision 4).
- **Hardening on every instance, not configurable down.** IMDSv2 required
  (`HttpTokens=required`), hop limit 1; EBS encryption on and
  `DeleteOnTermination` on every volume; no public IP; no instance profile
  (a later ADR may add one, explicitly). The IAM policy shipped in
  `deploy/provider-ec2/` requires each of these by condition key, so a
  provider bug cannot launch an instance without them (finding 5).
- **IMDS off after `GuestReady`.** Once the guest reports ready, the
  provider sets the instance's metadata endpoint to `disabled` (finding 7).
  User data, which may carry a join secret, is then unreadable from inside
  the guest by any process; the guest layer keeps its own copy for later
  boots (ADR-0175). Re-enabling IMDS is never automatic.
- **Observe.** `DescribeInstances` by instance ID for state and address,
  filtered by the ownership tag; `GuestReady` from the console line
  ADR-0175 defines, read with `GetConsoleOutput` `Latest` (finding 6). A
  booting instance is requeued on ADR-0043 Decision 8's cadence (2 s, then
  back-off), never blocking a worker. This is EC2's `GuestReady` transport,
  the choice ADR-0163 left to roadmap 24.
- **Power.** `desiredPowerState: PoweredOff` is `StopInstances` (compute
  stops billing; volumes do not). `On` is `StartInstances`.
- **Delete.** `TerminateInstances`, then confirm by tag that no volume of
  the instance remains, deleting any left (ADR-0158 Decision 6).
- **Capacity.** `InsufficientInstanceCapacity` and `Unsupported` (a type
  not offered in a zone) re-place the machine in another zone under
  ADR-0173, unchanged.
- **Provider status.** Zones (name and ID) that offer at least one mapped
  instance type, offered types per zone, and vCPU quota headroom per
  family, on ADR-0160 and ADR-0161's slow cadence.

### 4. A fake that rejects what EC2 rejects

Unit tests run against an in-process fake that **rejects** what the spike
saw AWS reject (`rules/testing.md` rule 1): a reused client token with
different parameters (`IdempotentParameterMismatch`); an instance type not
offered in the zone; user data over 16,384 bytes before encoding; a launch
without `HttpTokens=required`, with a hop limit above 1, with an
unencrypted volume or a public IP, or without the ownership tag (the
policy's denials); `InvalidInstanceID.NotFound` for a fresh instance ID on
its first describe; throttling with `RequestLimitExceeded`. A live tier
(`#[ignore]`) runs against a real account named by the environment and
fails loudly without one.

### 5. Provider class `ec2`

`Provider.spec.connection.ec2` carries the region and the role ARN to
assume (ADR-0159); `credentialsRef` is refused for it at admission, as for
`azure`. One `Provider` per account and region (ADR-0158 Decision 1).

### 6. A stolen credential is short-lived, bound to the site, and revocable

An STS role session cannot be revoked on its own, and deleting the provider
pod does not end one it holds. Neither is accepted as a gap; each is
bounded:

- **Sessions at the minimum.** `AssumeRoleWithWebIdentity` is called with
  `DurationSeconds=900`, STS's floor, and the session is refreshed at ten
  minutes. A copied session is good for at most fifteen minutes.
- **The token that mints sessions at the minimum too.** STS checks only the
  ServiceAccount token's signature and expiry, not whether its pod still
  exists, so a stolen token mints sessions until it expires. The operator
  projects it with `expirationSeconds: 600`, Kubernetes' floor (the kubelet
  refreshes it at 80 %), audience `sts.amazonaws.com`.
- **Bound to the site by default.** The shipped policies in
  `deploy/provider-ec2/` (runtime and publisher) deny every action unless
  `aws:SourceIp` is one of the site's egress addresses, or `aws:SourceVpce`
  is the site's VPC endpoint when calls go through one. A copied credential
  is useless from anywhere else. The guide makes this the default; a site
  that cannot name its egress removes the statement deliberately and
  records why.
- **Revocable as a whole.** The incident step is AWS's role-wide
  revocation: a deny on `aws:TokenIssueTime` before the moment of
  revocation, which ends every session issued until then (the console's
  "Revoke active sessions"). The provider simply assumes the role again;
  only stolen sessions stay dead.
- **Never at rest.** Token and session live in memory only, are never
  logged, and are never written to a file, an environment variable or a
  Kubernetes object (ADR-0159).

What remains, with source binding on, is misuse from inside the site's own
network for at most fifteen minutes.

## Consequences

- A second first-party cloud client, larger in protocol count than ARM
  (Query, REST, JSON 1.1, SigV4) but small in calls. SigV4 is the riskiest
  piece and is pinned by AWS's own test vectors plus the live tier.
- `quick-xml` joins the workspace; `cargo deny` must stay clean.
- One STS call per provider every ten minutes (Decision 6), and a site must
  know its egress addresses to keep source binding on. The operator's token
  projection for `ec2` differs from `azure`'s in expiry only.
- IMDS is off for the life of a ready instance: any guest software that
  expects IMDS after boot (the SSM agent, cloud-init modules) will not
  work, by design. An image that needs IMDS is a later, explicit choice.
- The kind name differs from CAPA's on purpose; documentation calls it out.
- Moving to the official crates later is a client-internal change behind
  the same trait.

## Alternatives considered

- **The official `aws-sdk-*` crates.** Least code, and AWS maintains them.
  Rejected for now: the smithy runtime is a large tree moving weekly, and
  ADR-0011's reason applies: a typed client over the few calls banlieue makes
  is easier to audit, fake and pin. Revisit if the call surface grows past
  what one crate can keep correct.
- **The `aws-sigv4` crate alone, with a first-party client.** Smaller than
  the full SDK, but still pulls the smithy runtime API crates for one
  function that AWS specifies completely with test vectors.
- **`AWSMachine` in banlieue's group.** Rejected: ambiguous `kubectl get`
  beside CAPA.
- **The default one-hour session, recorded as an accepted gap.** Rejected
  (Decision 6): every part of it can be bounded at little cost.
- **IMDS left on, or user data in IMDS for the life of the instance.**
  Rejected: any process in the guest, including a sandbox workload, could
  read the join secret at any time (finding 7 makes turning it off cheap).
- **`GuestReady` from EC2 status checks.** They report the hypervisor and
  the network stack, not that the image applied user data and its own
  readiness (ADR-0163 Decision 2).
