# Guide: Setting up the Kairos operator

`banlieue-imagebuilder` doesn't build OS images itself — it delegates the
OCI-pull and raw-disk-build step entirely to the
[Kairos operator](https://kairos.io), via the `OSArtifact` custom resource
(`build.kairos.io/v1alpha2`). This page gets that operator running in your
cluster, before you install `banlieue-imagebuilder` itself in
[Using banlieue-imagebuilder](using-banlieue-imagebuilder.md).

banlieue does not fork, vendor, or generate this CRD — the platform operator
installs kairos-operator independently, the same way you'd install any other
third-party operator your cluster depends on.

## Prerequisites

- A Kubernetes cluster with `kubectl` context pointed at it.

!!! info "No cert-manager or Helm chart required"
    Unlike some operators, the Kairos operator's own installation docs do not
    document a cert-manager prerequisite or a Helm chart — install is via
    `kubectl apply -k` (Kustomize) against the operator's own repo. If that
    changes upstream, `kubectl apply -k` will simply pull whatever the
    referenced `config/default` overlay declares.

## Install the operator

```sh
kubectl apply -k https://github.com/kairos-io/kairos-operator/config/default
```

This installs the operator's CRDs (including `osartifacts.build.kairos.io`,
the one `banlieue-imagebuilder` creates and watches) and the operator
Deployment itself in one pass.

## Verify

```sh
kubectl get pods -A | grep kairos
kubectl get crds | grep kairos.io
```

You should see the operator pod running, and `osartifacts.build.kairos.io` in
the CRD list. If it's missing, the operator isn't fully installed yet — check
`kubectl get kustomization` / operator pod logs for errors.

## Quick smoke test

Confirm the operator actually builds something before wiring
`banlieue-imagebuilder` up to it. This requests a `cloudImage` (raw disk) —
the same artifact kind `banlieue-imagebuilder` requests — from a small public
Kairos image:

```yaml title="smoke-test.yaml"
apiVersion: build.kairos.io/v1alpha2
kind: OSArtifact
metadata:
  name: smoke-test
spec:
  image:
    ref: quay.io/kairos/ubuntu:24.04-core-amd64-generic-v3.7.2
  artifacts:
    cloudImage: true
    arch: amd64
```

```sh
kubectl apply -f smoke-test.yaml
kubectl get osartifact smoke-test -w
```

!!! warning "Verify any Kairos image tag before using it"
    A wrong tag fails late and unhelpfully: the OSArtifact goes to `Error`
    only *after* kairos-operator has pulled the ~570MB `auroraboot` builder
    image (2+ minutes), and the real reason —
    `MANIFEST_UNKNOWN: manifest unknown` — appears only in the
    `pull-image-baseimage` **init container's** logs, not in the OSArtifact's
    own status.

    Kairos tags follow
    `<os-version>-<flavor>-<arch>-<model>-<kairos-version>`, and the two
    flavors differ in a way that trips people up:

    | Flavor | Shape | Contains |
    | --- | --- | --- |
    | `core` | `24.04-core-amd64-generic-v3.7.2` | base OS, **no** Kubernetes |
    | `standard` | `24.04-standard-amd64-generic-v3.7.2-k0s-v1.34.3-k0s.0` | base OS **plus** a bundled k8s distro |

    A `standard` tag *always* carries the k8s distro suffix — a bare
    `24.04-standard-amd64-generic-<version>` does not exist, however plausible
    it looks. Confirm a tag resolves before committing it to a manifest:

    ```sh
    REPO=kairos/ubuntu
    TAG=24.04-core-amd64-generic-v3.7.2
    TOKEN=$(curl -s "https://quay.io/v2/auth?service=quay.io&scope=repository:$REPO:pull" | jq -r .token)
    curl -s -o /dev/null -w '%{http_code}\n' -H "Authorization: Bearer $TOKEN" \
      -H "Accept: application/vnd.oci.image.index.v1+json" \
      "https://quay.io/v2/$REPO/manifests/$TAG"    # 200 = exists, 404 = does not
    ```

    Browse what actually exists at
    [quay.io/repository/kairos/ubuntu?tab=tags](https://quay.io/repository/kairos/ubuntu?tab=tags).

`status.phase` progresses `Pending -> Building -> Exporting -> Ready` (or
`Error`, with `status.message` set). Once it reaches `Ready`, the operator has
created a PVC named `smoke-test-artifacts` containing `smoke-test.raw` — the
same naming convention `banlieue-imagebuilder` relies on
(`VMImage.status.rawDiskArtifact.pvcRef` / `.diskFile`, see ADR-0010).

```sh
kubectl get pvc smoke-test-artifacts
```

Clean up the smoke test once confirmed:

```sh
kubectl delete osartifact smoke-test
kubectl delete pvc smoke-test-artifacts
```

With the operator side confirmed working, move on to
[Using banlieue-imagebuilder](using-banlieue-imagebuilder.md).
