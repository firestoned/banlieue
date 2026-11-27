<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# EC2 provider: IAM policies and role trust

| File | What it is |
| --- | --- |
| `runtime-policy.json` | The runtime provider's IAM policy: exactly the calls it makes ([ADR-0174](../../docs/adr/0174-ec2-inframachine-and-a-first-party-aws-client.md) Decisions 2 and 3, [ADR-0167](../../docs/adr/0167-nitrotpm-and-the-ek-public-key.md)), with every hardening rule required by a condition key. |
| `publisher-policy.json` | The image publisher's own IAM policy, for the publish Job ([ADR-0162](../../docs/adr/0162-publishing-a-vmimage-into-a-cloud.md) Decision 9). Attached to a **separate** role ([ADR-0159](../../docs/adr/0159-cloud-credentials-by-workload-identity-federation.md) Decision 3). |
| `source-binding.json` | Denies every action unless the request comes from the site's egress addresses or the site's VPC endpoint (ADR-0174 Decision 6). Attached to **both** roles by default. |
| `trust-policy.json` | The trust policy template for each role: one ServiceAccount of one cluster, by web identity. |
| `revoke-sessions.json` | The incident step: ends every session of a role issued before a given moment (ADR-0174 Decision 6). Attached only during an incident. |
| `rbac/clusterrole.yaml` | The provider's Kubernetes ClusterRole: read `Provider`s and own their status, realise `EC2Machine`s (no create or delete), list namespaces for a shared provider's selector, emit Events, read `VMImage`s and write their status rows (a `Template` source, a hand-published AMI ID, is read with `DescribeImages` and becomes a row for each `ec2` `Provider`: ready with the AMI's architecture, root device, boot mode and TPM support, or not ready as `AmiNotFound`, `AmiNotAvailable`, `AmiNotEbsBacked`, `AmiArchitectureUnsupported`; no finalizer, nothing is published). `ec2:DescribeImages` is already in `runtime-policy.json`. No Secret and no Job until the publisher lands (roadmap 24 phase D). |

All five passed AWS Access Analyzer's `validate-policy` (identity policies,
and the trust policy as `AWS::IAM::AssumeRolePolicyDocument`) with no
errors, security warnings or warnings. The shape of the runtime and
publisher policies (one statement per `RunInstances` resource type, tags
required on the request, IMDSv2 and hop limit, encrypted volumes, no public
IP, tagged images and security groups) was live-tested in a real account
during the roadmap 24 spike: Access Analyzer clean, 60 of 60 simulated
cases, and a live run that allowed every intended call and refused IMDSv1,
a hop limit of 2, an untagged launch and a public IP.

## Placeholders

The files carry placeholders, never real values. Replace every one before
creating anything:

| Placeholder | Replace with |
| --- | --- |
| `ACCOUNT_ID` | The 12-digit account the `Provider` runs in. |
| `REGION` | The `Provider`'s region, e.g. `ca-central-1`. Also inside `ec2.REGION.amazonaws.com`. |
| `PROVIDER_TAG_VALUE` | The `Provider`'s `banlieue-provider` tag value, `<namespace>.<name>` ([ADR-0158](../../docs/adr/0158-shape-of-a-cloud-provider-class.md) Decision 4), e.g. `banlieue-system.ec2-cc`. |
| `STAGING_BUCKET` | The admin-owned S3 staging bucket named on the `Provider`. |
| `VMIMPORT_ROLE_NAME` | The admin's VM Import service role (below). |
| `KMS_KEY_ID` | Only with a customer managed KMS key: its key ID. Otherwise delete the statement whose `Sid` starts `OptionalOnlyWithACustomerManagedKmsKey`. |
| `SITE_EGRESS_CIDR` | The site's public egress address(es), as CIDRs. Add more entries to the list for more than one. |
| `SITE_VPCE_ID` | The site's VPC endpoint ID (`vpce-...`), when calls go through one. |
| `ISSUER_HOST_PATH` | The cluster's ServiceAccount issuer URL without `https://`, e.g. `oidc.example.com/cluster`. |
| `SERVICEACCOUNT_NAMESPACE`, `SERVICEACCOUNT_NAME` | The ServiceAccount the role trusts (below). |
| `REVOKE_BEFORE` | ISO 8601 UTC time of revocation, e.g. `2026-11-26T12:00:00Z`. |

One way, keeping the checked-in files untouched:

```sh
mkdir -p /tmp/banlieue-ec2-iam
for f in deploy/provider-ec2/*.json; do
  sed -e "s/ACCOUNT_ID/$ACCOUNT_ID/g" -e "s/REGION/$REGION/g" \
      -e "s/PROVIDER_TAG_VALUE/$PROVIDER_TAG_VALUE/g" \
      -e "s/STAGING_BUCKET/$STAGING_BUCKET/g" \
      -e "s/VMIMPORT_ROLE_NAME/$VMIMPORT_ROLE_NAME/g" \
      -e "s/KMS_KEY_ID/$KMS_KEY_ID/g" \
      -e "s#SITE_EGRESS_CIDR#$SITE_EGRESS_CIDR#g" \
      -e "s/SITE_VPCE_ID/$SITE_VPCE_ID/g" \
      "$f" > "/tmp/banlieue-ec2-iam/$(basename "$f")"
done
```

The trust policy's placeholders differ per role; fill those per role.
Validate before creating anything:

```sh
aws accessanalyzer validate-policy --policy-type IDENTITY_POLICY \
  --policy-document file:///tmp/banlieue-ec2-iam/runtime-policy.json
aws accessanalyzer validate-policy --policy-type RESOURCE_POLICY \
  --validate-policy-resource-type AWS::IAM::AssumeRolePolicyDocument \
  --policy-document file:///tmp/banlieue-ec2-iam/trust-runtime.json
```

The trust policy reports one `SUGGESTION`, `CONFIRM_AUDIENCE_CLAIM_TYPE`:
it asks you to confirm that the single-valued `aud` claim is matched
without a `ForAnyValue`/`ForAllValues` qualifier, which it is. AWS's own
EKS trust policy shape returns the same suggestion.

## Credential

The provider's only cloud credential is a projected ServiceAccount token,
audience `sts.amazonaws.com`, `expirationSeconds: 600` (Kubernetes' floor;
the kubelet refreshes it at 80 %), re-read on every exchange. The provider
exchanges it with `AssumeRoleWithWebIdentity` for a session of
`DurationSeconds=900` (STS's floor) as the role in
`Provider.spec.connection.ec2`, and refreshes the session at ten minutes.
Token and session live in memory only: never logged, never written to a
file, an environment variable or a Kubernetes object. An `ec2` `Provider`
sets no `credentialsRef`; admission refuses one.

The operator renders that token for every `ec2` `Provider` it serves, and
`banlieue bootstrap provider ec2` does the same for a static install: a
projected volume mounted read-only at `/var/run/secrets/aws/tokens`, so the
file is `/var/run/secrets/aws/tokens/aws-identity-token`, named to the
provider by `AWS_WEB_IDENTITY_TOKEN_FILE` (the file `banlieue provider ec2`
reads by default). The workload gets the ClusterRole in
`rbac/clusterrole.yaml`, a Role for its own Lease, no Secret grant and no
Job Role.

The role's `MaxSessionDuration` can stay at its default (one hour): the
provider asks for 900 seconds, and a session is never longer than asked.

The runtime role trusts the provider pod's ServiceAccount:

```text
system:serviceaccount:<provider namespace>:banlieue-provider-<providerClassRef.name>-<provider name>
```

for example `system:serviceaccount:banlieue-system:banlieue-provider-ec2-ec2-cc`
for a `Provider` named `ec2-cc` of class `ec2` in `banlieue-system`. The name
is cut to 63 characters with a hash suffix when longer; read it from the
cluster (`kubectl -n <ns> get sa -l banlieue.io/provider=<name>`) rather
than deriving it by hand. A class with `workloadNamespace` set puts the
ServiceAccount there instead. A statically installed provider runs as
`banlieue-provider-<class>` in the install namespace.

The publisher role trusts the publish Job's ServiceAccount in the
imagebuild namespace (`banlieue-imagebuild`), which holds no Kubernetes
RBAC and mounts no API server token, as the Azure publisher's does.

## Creating the OIDC provider and the two roles

The cluster's ServiceAccount issuer must be public (its discovery document
and JWKS reachable by STS), as for Azure (ADR-0159). Register it once per
account:

```sh
aws iam create-open-id-connect-provider \
  --url "https://$ISSUER_HOST_PATH" \
  --client-id-list sts.amazonaws.com
```

Then, per role (`trust-runtime.json` and `trust-publisher.json` are
`trust-policy.json` with that role's ServiceAccount filled in):

```sh
aws iam create-role --role-name banlieue-ec2-provider \
  --assume-role-policy-document file:///tmp/banlieue-ec2-iam/trust-runtime.json
aws iam put-role-policy --role-name banlieue-ec2-provider \
  --policy-name runtime --policy-document file:///tmp/banlieue-ec2-iam/runtime-policy.json
aws iam put-role-policy --role-name banlieue-ec2-provider \
  --policy-name source-binding --policy-document file:///tmp/banlieue-ec2-iam/source-binding.json

aws iam create-role --role-name banlieue-ec2-publisher \
  --assume-role-policy-document file:///tmp/banlieue-ec2-iam/trust-publisher.json
aws iam put-role-policy --role-name banlieue-ec2-publisher \
  --policy-name publisher --policy-document file:///tmp/banlieue-ec2-iam/publisher-policy.json
aws iam put-role-policy --role-name banlieue-ec2-publisher \
  --policy-name source-binding --policy-document file:///tmp/banlieue-ec2-iam/source-binding.json
```

The role names are examples. Each policy also fits a customer managed
policy (6,144 characters) if you prefer `create-policy` and
`attach-role-policy`.

**Security groups and images.** `RunInstances` is allowed only in a security
group tagged `banlieue-provider` = `PROVIDER_TAG_VALUE`, and only from an
AMI owned by the account carrying the same tag. Tag each security group the
`Provider`'s network classes name; images published by banlieue carry the
tag already. An AMI banlieue did not publish needs that tag before the
provider can launch it. Subnets are scoped to the account: a subnet shared
from another account (AWS RAM) has that account's ARN, so widen the
`subnet` resource of `RunInstancesWithoutPublicIp` to its owner when you
use one.

## The image publisher

A `VMImage` published into EC2 is uploaded, imported and registered by a
Job, not by the provider (ADR-0162 Decision 9). Its role holds:

- **S3 write, no read.** `PutObject`, multipart upload and abort, `ListParts`
  and `ListMultipartUploads` (to adopt an upload a restarted Job left in
  flight), and `DeleteObject`, on `STAGING_BUCKET` only, and only when the
  bucket is in `ACCOUNT_ID` (`s3:ResourceAccount`). No `s3:GetObject`.
- **`iam:PassRole` on one role, to one service.** `VMIMPORT_ROLE_NAME`,
  only when passed to `vmie.amazonaws.com`. VM Import reads the object as
  that role; the publisher only writes it.
- **Import and register.** `ImportSnapshot` with the task tagged at
  creation; `CreateTags` on the imported snapshot (the task's tags do not
  reach it); `RegisterImage` tagged at creation, from a snapshot tagged as
  this `Provider`'s; `DeregisterImage` and `DeleteSnapshot` only on images
  and snapshots tagged `banlieue-provider` = `PROVIDER_TAG_VALUE`.

The VM Import service role and the staging bucket are the admin's, created
once, never by banlieue. The role trusts `vmie.amazonaws.com` with
`sts:ExternalId` = `vmimport`, and may read only the staging bucket (and
use the KMS key, when one is named). The bucket keeps block public access
on, with a lifecycle rule that expires objects after one day and aborts
incomplete multipart uploads.

## Source binding (on by default)

`source-binding.json` is one `Deny` of every action whose conditions are
ANDed, so a request is refused only when **all three** hold:

1. `NotIpAddress aws:SourceIp SITE_EGRESS_CIDR`: the caller's address is
   not the site's egress. `aws:SourceIp` is absent on a request made
   through a VPC endpoint, and a negated operator over an absent key
   evaluates to true, so this condition alone would refuse every VPC
   endpoint request.
2. `StringNotEqualsIfExists aws:SourceVpce SITE_VPCE_ID`: the request did
   not come through the site's VPC endpoint. On an internet request the key
   is absent and `IfExists` makes this true, leaving condition 1 to decide.
3. `Bool aws:ViaAWSService false`: the request was made by the principal
   itself, not by an AWS service on its behalf (a forward access session,
   e.g. EC2 calling KMS for an encrypted volume during `RunInstances`).
   AWS preserves the original source IP on such calls, but not the source
   VPC endpoint, and documents that a `aws:SourceVpce` condition must exempt
   them with `aws:ViaAWSService`.

So a call from the site's egress passes (1 is false), a call through the
site's endpoint passes (2 is false), and everything else, including a
copied session used from anywhere else, is denied, since an explicit deny
overrides every allow.

References: [AWS: Denies access to AWS based on the source IP](https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_policies_examples_aws_deny-ip.html),
and `aws:SourceIp`, `aws:SourceVpce` and `aws:ViaAWSService` in
[AWS global condition context keys](https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_policies_condition-keys.html)
("If the request comes from a host that uses an Amazon VPC endpoint, then
the `aws:SourceIp` key is not available").

Variants, by deletion:

- **Egress addresses only** (no VPC endpoint): delete the
  `StringNotEqualsIfExists` block. Every endpoint request is then refused,
  which is right when the site uses none.
- **VPC endpoint only**: delete the `NotIpAddress` block. Every internet
  request is then refused. Every AWS service the provider calls (EC2, S3,
  Service Quotas, the Tagging API) must then be reached through an endpoint
  in that VPC; list each endpoint's ID in the `aws:SourceVpce` array.

The binding governs what a session may do; it does not govern
`AssumeRoleWithWebIdentity` itself, which only the trust policy does.

**Removing it** is a deliberate act: a site that cannot name its egress
(dynamic addresses, a shared NAT it does not control) does not attach
`source-binding.json` to either role, and records why, with the date, in
its own change record. A copied session is then usable from anywhere for up
to fifteen minutes; ADR-0174 Decision 6 states that bound.

## Revocation (incident step)

An STS session cannot be revoked on its own. To end every session of a role
issued before now, set `REVOKE_BEFORE` to the current UTC time and attach
`revoke-sessions.json` to the role, as the console's "Revoke active
sessions" does:

```sh
sed "s/REVOKE_BEFORE/$(date -u +%Y-%m-%dT%H:%M:%SZ)/" \
  deploy/provider-ec2/revoke-sessions.json > /tmp/banlieue-ec2-iam/revoke.json
aws iam put-role-policy --role-name banlieue-ec2-provider \
  --policy-name revoke-older-sessions \
  --policy-document file:///tmp/banlieue-ec2-iam/revoke.json
```

Every session issued before that moment is refused from then on. The
provider simply assumes the role again on its next call; only stolen
sessions stay dead. A stolen ServiceAccount token can still mint sessions
until it expires (at most ten minutes): ten minutes after the first step,
run it again with the new time, which ends any session such a token minted
in between (the provider re-assumes once more), or rotate the cluster's
ServiceAccount signing key. Leave the policy attached: a later revocation
just moves the date.

## What these policies do not grant

- **No instance profile.** No `iam:PassRole` for the runtime role, and an
  explicit deny on `RunInstances` naming any instance profile. A later ADR
  may add one, explicitly.
- **No public IP.** `RunInstances` on a network interface or subnet is
  allowed only when `ec2:AssociatePublicIpAddress` is false. The provider
  always sends `AssociatePublicIpAddress=false`; keep auto-assign public IP
  off on the subnets too, since a request that omits the parameter is not
  refused by the policy.
- **No IMDSv1.** `RunInstances` requires `HttpTokens=required` and a hop
  limit of 1; the provider turns IMDS off after `GuestReady`.
- **IMDS can only be turned off, never back on.** `ModifyInstanceMetadataOptions`
  is allowed only when the request sets `HttpEndpoint=disabled` and changes
  nothing else. The condition is on `ec2:Attribute/HttpEndpoint`, the value in
  the request: `ec2:MetadataHttpEndpoint` carries the instance's *current*
  setting, so a rule on it would allow re-enabling a disabled endpoint and
  refuse the disable. Each other option the call can change is refused by a
  `Null` condition: `ec2:Attribute` is single-valued (EC2 authorizes the call
  one attribute at a time), so without them a rider on the disable request
  goes through. Verified live: enable
  refused with IMDS on and off; a disable carrying `HttpTokens=optional`, a
  hop limit or instance tags refused; a plain disable allowed.
- **No unencrypted volume.** Every volume of a launch must be encrypted.
- **No S3 read for the publisher**, and no S3 at all for the runtime role.
- **No image or snapshot write for the runtime role**, and no instance
  operation for the publisher role.
- **No image or snapshot sharing.** No `ModifyImageAttribute` or
  `ModifySnapshotAttribute`.
- **Nothing outside `REGION`** for the describe and tagging reads; every
  other grant names `REGION` in its resource ARN.
- **No KMS** unless a customer managed key is used, and then only through
  EC2 (`kms:ViaService`), on that key.
