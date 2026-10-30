<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# 0110 — The registry push Job trusts an operator-supplied CA bundle

- **Status:** Accepted
- **Date:** 2026-10-29
- **Deciders:** Erick Bourgeois
- **Related:** [ADR-0064](0064-artifact-delivery-to-host-resident-providers.md)
  (the push Job and its configuration: this adds one knob to it),
  [ADR-0022](0022-vsphere-iso-overlay-files.md) (the precedent: a
  mirrored install must be able to repoint images it cannot pull from the
  public default),
  [ADR-0008](0008-byoc-vsphere-http-client.md) (banlieue owns the transport,
  so it owns TLS trust)

## Context

ADR-0064 delivers a built disk to a Cloud Hypervisor host through an OCI
registry: a **push Job** in the build namespace uploads it, and the host
pulls it by digest. The Job runs the banlieue image, which is distroless and
carries only the public trust roots. A registry served by a private or
corporate CA fails the push with

```
invalid peer certificate: UnknownIssuer
```

and nothing in the imagebuilder's configuration can fix it: the Job spec is
generated, with no way to mount a CA bundle or set an environment variable.
The first real install (an internal registry behind a corporate CA) hit
exactly this, one step after a second mirrored-install problem: the push
Job's image defaults to the public registry (`--push-image`), which a
cluster that mirrors images cannot pull from. That one is configurable
already, but only the ADR-0022 importer image was documented as needing it.

The host side is not affected: the host pulls with the host's own trust
store, which the host owner already maintains for everything else on it.

## Decision

### 1. A CA bundle, named like the operator's

The imagebuilder takes the CA bundle from a ConfigMap **or** a Secret in the
build namespace, the same two shapes `Provider.spec.connection` offers
(`caBundle` by ConfigMap or Secret reference):

| Flag | Env | Meaning |
| --- | --- | --- |
| `--registry-ca-bundle-config-map` | `BANLIEUE_REGISTRY_CA_BUNDLE_CONFIG_MAP` | ConfigMap holding the PEM |
| `--registry-ca-bundle-secret` | `BANLIEUE_REGISTRY_CA_BUNDLE_SECRET` | Secret holding the PEM (exclusive with the ConfigMap) |
| `--registry-ca-bundle-key` | `BANLIEUE_REGISTRY_CA_BUNDLE_KEY` | key within it; default `ca.crt` |

Unset, nothing changes: the Job trusts the image's public roots as before.

### 2. The Job mounts the one key, read-only, and gets a flag

The push Job mounts **only the named key** (a volume `items` entry) read-only
at `/var/run/banlieue/registry-ca`, and runs
`banlieue imagebuilder push --ca-bundle /var/run/banlieue/registry-ca/<key>`.
It needs no new RBAC: the kubelet mounts the object, not the Job and not the
imagebuilder.

### 3. The client adds the bundle as roots; it does not rely on the environment

`banlieue-oci` gains one function that builds the `reqwest::Client`, adding
every certificate in the PEM as a root (`Certificate::from_pem_bundle` and
`ClientBuilder::add_root_certificate`), the way the vSphere and Proxmox
clients already do. It was chosen over mounting the file and setting
`SSL_CERT_FILE` because that variable's effect belongs to whichever
verifier reqwest's rustls feature set selects, which is a dependency detail
that a minor upgrade can change and that no test here can pin without a
registry on a private CA. An explicit root is the same on every platform.

An **empty bundle is refused**. `from_pem_bundle` returns no error for input
with no PEM block, and accepting it would leave the client on the public
roots while the operator believes a CA is pinned. The Job fails with a
message naming the file instead.

The bundle **adds** roots; it does not replace the public ones, and it does
not disable verification. There is no `insecure` option and none is added:
an operator who cannot supply the CA uses `--registry-plain-http` on a
private network, which is explicit and already exists.

### 4. Validation at startup

The key is checked when the flags are resolved, before any Job exists: a
non-empty file name of letters, digits, `.`, `_` and `-`, not `.` or `..`.
It becomes a path component and a volume item path, so it must not be able
to leave the mount. The ConfigMap and the Secret options conflict at parse
time.

### 5. The push image: documented, not changed

`--push-image` / `BANLIEUE_PUSH_IMAGE` already exists and a mirrored install
must set it; it is now documented beside the other registry settings. The
default stays the public image. Making the default follow the imagebuilder's
own image was considered: the imagebuilder cannot read its own image from the
API without a new Pod `get` grant, and passing it by the Deployment's
environment is the same configuration the operator would set by hand. Left
as a follow-up (see Consequences).

## Consequences

- A registry behind a private CA works with three settings and no image
  change. The imagebuilder must be restarted to read them, as for the other
  registry settings.
- The push Job gains a trust anchor of the operator's choosing. It can only
  widen what the Job accepts to the CAs the operator named, in a Job that
  holds one registry credential and no Kubernetes token. The host still
  pulls only a digest in the one repository its own root-owned config names,
  so a rogue CA on the push side cannot change what a host boots.
- Follow-up: default `--push-image` to the imagebuilder's own image (the
  Deployment would pass it through the downward API or an env var), so a
  mirrored install needs one setting fewer. Not done here: it changes a
  default and needs its own test of how a digest-pinned deployment image is
  reported.
- Threat model: a new trust anchor in the push Job (TB-4/TB-6), pass recorded
  in the same change.
