<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# Private-CA image pulls for kairos-operator OSArtifact builds (no `insecure`)

> **Status:** design / upstream-contribution note (off to the side; not an ADR).
> **Audience:** whoever runs `banlieue-imagebuilder` against an internal OCI
> registry fronted by a **private CA**, in an environment where disabling TLS
> verification is not acceptable.

## Problem

`banlieue-imagebuilder` turns a `VMImage` with a `kind: Url` source into a raw
disk by server-side-applying a kairos-operator `OSArtifact` whose
`spec.image.ref` is the source OCI image (see
`crates/banlieue-imagebuilder/src/reconciler/vmimage.rs::desired_os_artifact`).
kairos-operator then runs **`auroraboot unpack <ref> /rootfs`** in an init
container (`pull-image-baseimage`) to fetch that image.

`auroraboot` pulls with **go-containerregistry**, which trusts the **container's
system certificate pool** (or `SSL_CERT_FILE` / `SSL_CERT_DIR`). When the source
image lives on an internal mirror whose TLS cert chains to a **private CA**, the
pull fails:

```
failed to unpack image: dumping the source image <registry-mirror>/... to /rootfs:
  Get "https://<registry-mirror>/v2/": tls: failed to verify certificate:
  x509: certificate signed by unknown authority
```

The requirement here is to make that pull **verify against the private CA** —
**not** to disable verification.

## Why the existing kairos-operator knobs do **not** solve this

Verified against the kairos-operator source (HEAD, 2026-08):

| Knob | Where it applies | Covers the `image.ref` unpack pull? |
| --- | --- | --- |
| `spec.caCertificatesVolume` (mounts a CA at `/etc/ssl/buildah/certs`) | **buildah OCI-build** container only — used when `spec.ociSpec.ref` is set (build-from-Dockerfile), i.e. `image.ref` empty | **No** |
| `spec.buildEnv` (e.g. `HTTP_PROXY`) | OCI-build container only; "ignored when using a pre-built image" | **No** |
| `spec.pullInsecureRegistry` → `auroraboot unpack --allow-insecure-registries` | the unpack path | Yes, but it **disables** verification — rejected |
| `spec.imageCredentialsSecretRef` | sets `DOCKER_CONFIG` on the unpack container | Auth only — **not** TLS/CA |
| `spec.volumes` | "importers and the build pod" | **Not** mounted into the unpack container |

The decisive evidence is `internal/controller/job.go`,
`unpackAndPackToArtifactsContainer(...)` — the container that performs the
`image.ref` pull:

```go
// name: "pull-image-baseimage", image: toolImage (auroraboot)
volMounts := []corev1.VolumeMount{
    {Name: rootfsVolumeName,    MountPath: rootfsMountPath},
    {Name: artifactsVolumeName, MountPath: artifactsMountPath},
}
env := []corev1.EnvVar{}
if artifact.Spec.Image.ImageCredentialsSecretRef != nil {
    // ... mounts docker creds, sets DOCKER_CONFIG (auth only)
}
```

No CA volume, no `SSL_CERT_FILE`. So **for banlieue's path (`image.ref` →
`auroraboot unpack`) there is no CRD-level way to inject a CA.** Upgrading
kairos-operator does not change this; the CA knobs it has are build-path-only.

---

## Part A — Upstream kairos-operator change (the proper fix)

Make the **unpack** path able to trust a private CA, mirroring what
`caCertificatesVolume` already does for the buildah path. Because
go-containerregistry honors `SSL_CERT_FILE`, the minimal change is to mount a
CA bundle into the unpack container and point `SSL_CERT_FILE` at it — no
`update-ca-certificates`, no base-image assumptions.

### A.1 CRD (`api/v1alpha2/osartifact_types.go`)

Add a field on `OSArtifactSpec` (name mirrors the existing one):

```go
// UnpackCACertificatesVolume names a volume (from spec.volumes) whose contents
// are mounted read-only into the auroraboot unpack container; SSL_CERT_FILE is
// pointed at <mount>/<file> so the pull of a private-CA image.ref (and of
// artifacts.bundles) verifies against it. Use instead of pullInsecureRegistry
// when the source registry uses a private/enterprise CA.
// +optional
UnpackCACertificatesVolume string `json:"unpackCACertificatesVolume,omitempty"`

// UnpackCACertificatesFile is the file name within UnpackCACertificatesVolume
// to set SSL_CERT_FILE to. Defaults to "ca.crt".
// +optional
UnpackCACertificatesFile string `json:"unpackCACertificatesFile,omitempty"`
```

Then `make manifests generate` (regenerate CRD + deepcopy).

### A.2 Controller (`internal/controller/job.go`)

In `unpackAndPackToArtifactsContainer(...)` (and the bundle unpack helper
`unpackContainer(...)`), when the field is set:

```go
const unpackCAMountPath = "/etc/ssl/unpack-ca"

if v := artifact.Spec.UnpackCACertificatesVolume; v != "" {
    file := artifact.Spec.UnpackCACertificatesFile
    if file == "" { file = "ca.crt" }
    volMounts = append(volMounts, corev1.VolumeMount{
        Name:      v,                 // must exist in spec.volumes
        MountPath: unpackCAMountPath,
        ReadOnly:  true,
    })
    env = append(env, corev1.EnvVar{
        Name:  "SSL_CERT_FILE",
        Value: filepath.Join(unpackCAMountPath, file),
    })
}
```

The named volume already flows through `spec.volumes` (validated by
`OSArtifactSpec.Validate`), so no new volume plumbing is needed — only the mount
+ env on the unpack container(s).

> **Note on `SSL_CERT_FILE` semantics:** Go's `crypto/x509` treats
> `SSL_CERT_FILE` as the *sole* bundle (it does not append to the system pool).
> The mounted bundle must therefore contain the full chain the unpack step needs
> (the private root/intermediates — plus any public roots if the same build also
> pulls from public registries). For a purely-internal mirror, the private CA
> chain is sufficient. If mixing is required, prefer `SSL_CERT_DIR` with a
> directory that includes both, or have the operator append rather than replace.

### A.3 Usage once merged

```yaml
apiVersion: build.kairos.io/v1alpha2
kind: OSArtifact
spec:
  image:
    ref: <registry-mirror>/kairos/ubuntu:24.04-core-amd64-generic-v3.7.2
  unpackCACertificatesVolume: private-ca
  unpackCACertificatesFile: ca.crt
  volumes:
    - name: private-ca
      configMap:
        name: private-ca-bundle      # key: ca.crt (PEM)
  artifacts: { cloudImage: true, arch: amd64 }
```

`banlieue-imagebuilder` would then set `unpackCACertificatesVolume` +
`volumes[]` in `desired_os_artifact` (gated on a configured CA ConfigMap),
giving verified TLS with **no** `insecure`.

---

## Part B — What banlieue can do **now** (no `insecure`, no operator change)

The unpack container's image is the operator's `--tool-image` (auroraboot).
go-containerregistry trusts that image's **own** system cert pool. So: **use an
auroraboot image that already trusts the private CA**, and point `--tool-image`
at it. This needs no CRD field and no operator code change.

### B.1 Build a CA-trusting auroraboot (same pattern the OS images already use)

This is the **same CA-baking recipe the platform's `vm-build` images already
use** (`COPY <ca> → update-ca-certificates → ENV SSL_CERT_FILE=…`), applied to
the auroraboot builder. It bases off the **internal mirror's** auroraboot, so it
pulls **nothing from outside**:

```dockerfile
# Base off the MIRRORED auroraboot (no external pull); match --tool-image version.
FROM <registry-mirror>/kairos/auroraboot:v0.24.0

# Private CA chain (root + intermediates), PEM. Supplied via build context;
# never commit the bytes to a public repo. Same file the vm-build images use.
COPY private-ca-bundle.pem /usr/local/share/ca-certificates/private-ca-bundle.crt

# Prefer the distro trust tool when present; the appended bundle + SSL_CERT_FILE
# is the portable guarantee go-containerregistry (auroraboot) honors regardless
# of the auroraboot base distro.
RUN update-ca-certificates 2>/dev/null \
 || cat /usr/local/share/ca-certificates/private-ca-bundle.crt >> /etc/ssl/certs/ca-certificates.crt
ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
```

> **Empirically confirmed (2026-08-08).** Building an `OSArtifact` from an
> internal source image (`<sc90-mirror>/vm-images/rhel98-kairos:v0.3.0`) with the
> *stock* mirrored auroraboot as `--tool-image` fails **only** at the source
> pull with `x509: certificate signed by unknown authority` — the source image
> itself is valid and reachable. Baking the CA into the tool-image per above
> resolves exactly that step, with no `--allow-insecure-registries`.
>
> If the auroraboot base lacks `update-ca-certificates`, the `||` fallback
> appends to the existing bundle; `SSL_CERT_FILE` then points auroraboot at it.
> (openSUSE anchors live at `/etc/pki/trust/anchors`, RHEL at
> `/etc/pki/ca-trust/source/anchors` — adjust the `COPY` target if you prefer
> the distro's native anchor dir over the appended-bundle approach.)

Build + push is a **registry/image operation the platform owner performs**
(banlieue never builds or pushes images):

```sh
docker build -t <registry-mirror>/kairos/auroraboot:v0.24.0-privateca \
  --build-context . .
docker push <registry-mirror>/kairos/auroraboot:v0.24.0-privateca
```

### B.2 Point kairos-operator at it

Set the operator's builder image to the CA-trusting one (the flag banlieue
already relies on for mirroring):

```
--tool-image=<registry-mirror>/kairos/auroraboot:v0.24.0-privateca
```

This can be wired in the kairos-operator install (see `bootstrap-kairos-operator.sh`,
which should gain a `TOOL_IMAGE` env + mirror rewriting for on-prem — tracked
separately). Once set, every OSArtifact unpack — including the ones
`banlieue-imagebuilder` creates — verifies the mirror against the private CA.

### B.3 Why `banlieue-imagebuilder` itself cannot fix this today

`desired_os_artifact` can only set `OSArtifact` **spec** fields, and (per the
table above) none of them inject a CA into the unpack container on the current
CRD. So the fix must live in the **tool-image** (B.1/B.2) until Part A lands
upstream; then banlieue can set `unpackCACertificatesVolume` directly.

## Recommendation

- **Now:** B.1 + B.2 — a CA-trusting auroraboot `--tool-image`. Verified TLS, no
  `insecure`, no operator fork.
- **Upstream:** open the Part A PR so a CA can be supplied per-OSArtifact via a
  volume + `SSL_CERT_FILE`; then `banlieue-imagebuilder` wires it automatically.
