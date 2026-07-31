#!/usr/bin/env bash
# Bootstraps kairos-operator onto an existing Kubernetes cluster, plus the
# default StorageClass its builds depend on, so banlieue-imagebuilder has
# something to drive OSArtifact builds against (ADR-0010).
#
# KUBECONFIG is deliberately NOT set here -- export it yourself so this script
# can never surprise you by acting on a cluster you didn't mean:
#
#   export KUBECONFIG=~/dev/kubeconfig/dev-cluster.yaml
#   ./scripts/bootstrap-kairos-operator.sh all
#
# Every step is idempotent: re-running skips what already exists.
#
# Ordering matters and is the whole reason this is a script rather than a
# list of commands in a guide. kairos-operator creates a PVC for each
# OSArtifact's output *without* a storageClassName, so it binds to whatever
# the cluster's default StorageClass is AT CREATION TIME. A PVC created
# before a default exists gets an empty storageClassName -- and that field is
# immutable, so the claim can never bind and the OSArtifact hangs forever
# with a "no persistent volumes available for this claim and no storage class
# is set" event. Storage therefore has to be in place, marked default, and
# actually running before the first OSArtifact is created.
#
# Usage:
#   ./bootstrap-kairos-operator.sh [all|storage|operator|smoke|status|destroy]
#
# All settings below can be overridden via environment variables.
set -euo pipefail

# Rancher local-path-provisioner: the least-effort way to get a working
# default StorageClass on a bare cluster (hostPath-backed, no external
# storage). Set INSTALL_STORAGE=false if the cluster already has a default
# StorageClass you'd rather use.
INSTALL_STORAGE="${INSTALL_STORAGE:-true}"
LOCAL_PATH_VERSION="${LOCAL_PATH_VERSION:-v0.0.31}"
LOCAL_PATH_URL="${LOCAL_PATH_URL:-https://raw.githubusercontent.com/rancher/local-path-provisioner/${LOCAL_PATH_VERSION}/deploy/local-path-storage.yaml}"
LOCAL_PATH_NAMESPACE="${LOCAL_PATH_NAMESPACE:-local-path-storage}"
STORAGE_CLASS="${STORAGE_CLASS:-local-path}"

# kairos-operator installs via kustomize, not Helm -- there is no published
# Helm chart, despite the operator's own naming suggesting otherwise. Verified
# against https://kairos.io/operator-docs/installation/ (2026-07). No
# cert-manager prerequisite is documented either.
KAIROS_OPERATOR_REF="${KAIROS_OPERATOR_REF:-https://github.com/kairos-io/kairos-operator/config/default}"
KAIROS_NAMESPACE="${KAIROS_NAMESPACE:-operator-system}"
KAIROS_DEPLOYMENT="${KAIROS_DEPLOYMENT:-operator-kairos-operator}"

# Smoke-test OSArtifact: requests the same `cloudImage` (raw disk) artifact
# banlieue-imagebuilder asks for, so a passing smoke test means the exact path
# banlieue depends on works.
SMOKE_NAME="${SMOKE_NAME:-smoke-test}"
SMOKE_IMAGE="${SMOKE_IMAGE:-quay.io/kairos/ubuntu:24.04-core-amd64-generic-v3.7.2}"
SMOKE_ARCH="${SMOKE_ARCH:-amd64}"
# The auroraboot builder image is ~570MB, so the first run is dominated by the
# pull. 90 * 10s = 15min.
SMOKE_WAIT_ATTEMPTS="${SMOKE_WAIT_ATTEMPTS:-90}"
SMOKE_WAIT_INTERVAL="${SMOKE_WAIT_INTERVAL:-10}"

# Timeout for `kubectl wait` on Deployment availability.
WAIT_TIMEOUT="${WAIT_TIMEOUT:-300s}"

log() { echo "==> $*" >&2; }
warn() { echo "!!! $*" >&2; }

check_deps() {
  command -v kubectl >/dev/null 2>&1 || { warn "kubectl not found in PATH"; exit 1; }
  if ! kubectl version -o json >/dev/null 2>&1 && ! kubectl cluster-info >/dev/null 2>&1; then
    warn "Cannot reach a cluster. Set KUBECONFIG (this script never sets it for you):"
    warn "  export KUBECONFIG=/path/to/kubeconfig"
    exit 1
  fi
}

# Print exactly which cluster is about to be modified. Cheap insurance against
# running this against the wrong context.
show_target() {
  local ctx server
  ctx="$(kubectl config current-context 2>/dev/null || echo '<none>')"
  server="$(kubectl config view --minify -o jsonpath='{.clusters[0].cluster.server}' 2>/dev/null || echo '<unknown>')"
  log "Target cluster: context=$ctx server=$server"
}

# Every node carrying a NoSchedule taint means nothing without a matching
# toleration can be scheduled anywhere -- which is the state a k0s cluster
# lands in when all its nodes are controller+worker and `noTaints` was not
# set. Neither local-path-provisioner nor kairos-operator ships tolerations,
# so they would sit Pending indefinitely with no obvious error. Fail fast with
# the fix instead.
preflight_scheduling() {
  local total tainted
  total="$(kubectl get nodes --no-headers 2>/dev/null | wc -l | tr -d ' ')"
  # `$` is the template ROOT (the List), not the current node -- capture the
  # name into $n inside the outer range or every line comes out "<no value>",
  # which dedupes to a single line and silently defeats the comparison below.
  tainted="$(kubectl get nodes -o go-template='{{range .items}}{{$n := .metadata.name}}{{range .spec.taints}}{{if eq .effect "NoSchedule"}}{{$n}}{{"\n"}}{{end}}{{end}}{{end}}' 2>/dev/null | sort -u | grep -c . || true)"
  if [[ "$total" -gt 0 && "$tainted" -eq "$total" ]]; then
    warn "All $total node(s) carry a NoSchedule taint -- nothing schedulable can run."
    warn "kairos-operator and the storage provisioner will sit Pending forever."
    warn ""
    warn "For an all-controller+worker k0s cluster, either rebuild with"
    warn "noTaints (scripts/bootstrap-k0s-cluster.sh now sets this), or lift"
    warn "the taint on the running cluster:"
    warn ""
    warn "  kubectl taint nodes --all node-role.kubernetes.io/control-plane- "
    warn ""
    warn "Set SKIP_PREFLIGHT=true to bypass this check."
    [[ "${SKIP_PREFLIGHT:-false}" == "true" ]] || exit 1
  fi
}

# Name of the current default StorageClass, if any.
default_storage_class() {
  kubectl get storageclass \
    -o jsonpath='{range .items[?(@.metadata.annotations.storageclass\.kubernetes\.io/is-default-class=="true")]}{.metadata.name}{"\n"}{end}' \
    2>/dev/null | head -n1
}

install_storage() {
  if [[ "$INSTALL_STORAGE" != "true" ]]; then
    log "INSTALL_STORAGE=$INSTALL_STORAGE -- skipping storage provisioner"
  else
    log "Installing local-path-provisioner $LOCAL_PATH_VERSION"
    kubectl apply -f "$LOCAL_PATH_URL"
  fi

  # Mark our class default only if the cluster has no default at all --
  # never silently steal the role from an existing one.
  local current
  current="$(default_storage_class)"
  if [[ -z "$current" ]]; then
    log "No default StorageClass set; marking $STORAGE_CLASS as default"
    kubectl patch storageclass "$STORAGE_CLASS" -p \
      '{"metadata":{"annotations":{"storageclass.kubernetes.io/is-default-class":"true"}}}'
  elif [[ "$current" == "$STORAGE_CLASS" ]]; then
    log "$STORAGE_CLASS is already the default StorageClass"
  else
    log "Default StorageClass is already '$current' -- leaving it alone"
  fi

  # Verify rather than assume: the patch above failing (or being skipped)
  # is exactly what leaves OSArtifact PVCs permanently unbindable.
  current="$(default_storage_class)"
  if [[ -z "$current" ]]; then
    warn "Still no default StorageClass. OSArtifact PVCs will never bind."
    exit 1
  fi
  log "Default StorageClass: $current"

  if [[ "$INSTALL_STORAGE" == "true" ]]; then
    log "Waiting for the provisioner to become available"
    if ! kubectl -n "$LOCAL_PATH_NAMESPACE" wait --for=condition=Available \
         deployment --all --timeout="$WAIT_TIMEOUT"; then
      warn "Provisioner did not become available. Check scheduling:"
      warn "  kubectl -n $LOCAL_PATH_NAMESPACE describe pods"
      exit 1
    fi
  fi
}

install_operator() {
  log "Installing kairos-operator from $KAIROS_OPERATOR_REF"
  kubectl apply -k "$KAIROS_OPERATOR_REF"

  log "Waiting for $KAIROS_DEPLOYMENT in $KAIROS_NAMESPACE"
  if ! kubectl -n "$KAIROS_NAMESPACE" rollout status \
       "deployment/$KAIROS_DEPLOYMENT" --timeout="$WAIT_TIMEOUT"; then
    warn "kairos-operator did not roll out. Check:"
    warn "  kubectl -n $KAIROS_NAMESPACE describe pods"
    exit 1
  fi

  # The CRD banlieue-imagebuilder creates and watches. If this is missing the
  # operator install is incomplete, whatever the Deployment says.
  if ! kubectl get crd osartifacts.build.kairos.io >/dev/null 2>&1; then
    warn "CRD osartifacts.build.kairos.io not found -- operator install incomplete"
    exit 1
  fi
  log "kairos-operator ready (osartifacts.build.kairos.io registered)"
}

smoke_test() {
  # Recreate from scratch: a leftover OSArtifact from a previous run may own a
  # PVC that was created before a default StorageClass existed, and a PVC's
  # storageClassName is immutable -- such a claim can never bind, so reusing it
  # would fail forever for a reason that has nothing to do with this run.
  if kubectl -n "$KAIROS_NAMESPACE" get osartifact "$SMOKE_NAME" >/dev/null 2>&1; then
    log "Removing previous $SMOKE_NAME OSArtifact"
    kubectl -n "$KAIROS_NAMESPACE" delete osartifact "$SMOKE_NAME" --wait=true || true
  fi
  kubectl -n "$KAIROS_NAMESPACE" delete pvc "${SMOKE_NAME}-artifacts" \
    --ignore-not-found >/dev/null 2>&1 || true

  log "Creating OSArtifact $SMOKE_NAME (cloudImage from $SMOKE_IMAGE)"
  kubectl apply -f - <<EOF
apiVersion: build.kairos.io/v1alpha2
kind: OSArtifact
metadata:
  name: $SMOKE_NAME
  namespace: $KAIROS_NAMESPACE
spec:
  image:
    ref: $SMOKE_IMAGE
  artifacts:
    cloudImage: true
    arch: $SMOKE_ARCH
EOF

  log "Waiting for the build (first run pulls a ~570MB builder image)"
  local phase="" i
  for i in $(seq 1 "$SMOKE_WAIT_ATTEMPTS"); do
    phase="$(kubectl -n "$KAIROS_NAMESPACE" get osartifact "$SMOKE_NAME" \
      -o jsonpath='{.status.phase}' 2>/dev/null || true)"
    case "$phase" in
      Ready)
        log "OSArtifact reached Ready"
        kubectl -n "$KAIROS_NAMESPACE" get pvc "${SMOKE_NAME}-artifacts" || true
        log "Smoke test PASSED -- clean up with: $0 destroy"
        return 0
        ;;
      Error)
        warn "OSArtifact reached Error"
        smoke_diagnostics
        return 1
        ;;
    esac
    [[ $((i % 6)) -eq 0 ]] && log "  ... phase=${phase:-<none>} (${i}/${SMOKE_WAIT_ATTEMPTS})"
    sleep "$SMOKE_WAIT_INTERVAL"
  done

  warn "Timed out with phase=${phase:-<none>}"
  smoke_diagnostics
  return 1
}

# Dump what actually matters when a build fails. The builder Pod runs
# `auroraboot unpack` in an init container (pull-image-baseimage) before the
# main build-cloud-image container, so an Init:Error is a pull/unpack failure,
# not a disk-build failure -- and its logs are the only place the real reason
# appears.
smoke_diagnostics() {
  local pod
  warn "--- OSArtifact status ---"
  kubectl -n "$KAIROS_NAMESPACE" get osartifact "$SMOKE_NAME" -o yaml 2>/dev/null \
    | sed -n '/^status:/,$p' >&2 || true

  warn "--- PVC ---"
  kubectl -n "$KAIROS_NAMESPACE" get pvc "${SMOKE_NAME}-artifacts" >&2 2>&1 || true
  kubectl -n "$KAIROS_NAMESPACE" describe pvc "${SMOKE_NAME}-artifacts" 2>/dev/null \
    | sed -n '/Events:/,$p' >&2 || true

  pod="$(kubectl -n "$KAIROS_NAMESPACE" get pods \
    -l "build.kairos.io/artifact=$SMOKE_NAME" \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)"
  if [[ -z "$pod" ]]; then
    warn "No builder pod found (label build.kairos.io/artifact=$SMOKE_NAME)"
    return
  fi

  warn "--- builder pod $pod ---"
  kubectl -n "$KAIROS_NAMESPACE" get pod "$pod" -o wide >&2 2>&1 || true
  kubectl -n "$KAIROS_NAMESPACE" describe pod "$pod" 2>/dev/null \
    | sed -n '/Events:/,$p' >&2 || true

  local c logs all_logs=""
  for c in pull-image-baseimage build-cloud-image; do
    warn "--- logs: $c ---"
    logs="$(kubectl -n "$KAIROS_NAMESPACE" logs "$pod" -c "$c" --tail=50 2>&1 || true)"
    echo "$logs" >&2
    all_logs+="$logs"$'\n'
  done

  # Translate the failure signatures we've actually hit into the fix, rather
  # than leaving the next person to re-derive them from a wall of debug output.
  if grep -qiE '/dev/loop|loop device' <<<"$all_logs"; then
    warn ""
    warn "HINT: the builder could not open a loop device."
    warn "A privileged container only sees host device nodes that existed when"
    warn "the container was CREATED, and the kernel's loop module autoloads on"
    warn "first /dev/loop-control access -- i.e. a moment too late. Load it at"
    warn "boot on every node that may run a build:"
    warn "  sudo modprobe loop && echo loop | sudo tee /etc/modules-load.d/loop.conf"
    warn "(scripts/bootstrap-k0s-cluster.sh now does this via cloud-init, so"
    warn " freshly-built clusters are unaffected.)"
  fi
  if grep -qiE 'MANIFEST_UNKNOWN|manifest unknown' <<<"$all_logs"; then
    warn ""
    warn "HINT: the source image tag does not exist in the registry."
    warn "Kairos tags are <os>-<flavor>-<arch>-<model>-<kairos-version>, and a"
    warn "'standard' tag ALWAYS carries a bundled k8s distro suffix (e.g."
    warn "...-v3.7.2-k0s-v1.34.3-k0s.0). Browse real tags at:"
    warn "  https://quay.io/repository/kairos/ubuntu?tab=tags"
  fi
}

status() {
  echo "--- default StorageClass ---"
  kubectl get storageclass 2>&1 || true
  echo
  echo "--- kairos-operator ---"
  kubectl -n "$KAIROS_NAMESPACE" get pods 2>&1 || true
  echo
  echo "--- OSArtifacts ---"
  kubectl get osartifacts -A 2>&1 || true
}

destroy() {
  log "Removing smoke-test artifacts"
  kubectl -n "$KAIROS_NAMESPACE" delete osartifact "$SMOKE_NAME" --ignore-not-found || true
  kubectl -n "$KAIROS_NAMESPACE" delete pvc "${SMOKE_NAME}-artifacts" --ignore-not-found || true
  log "kairos-operator itself left installed; remove it with:"
  log "  kubectl delete -k $KAIROS_OPERATOR_REF"
}

main() {
  local cmd="${1:-all}"
  case "$cmd" in
    storage)  check_deps; show_target; preflight_scheduling; install_storage ;;
    operator) check_deps; show_target; preflight_scheduling; install_operator ;;
    smoke)    check_deps; show_target; smoke_test ;;
    status)   check_deps; show_target; status ;;
    destroy)  check_deps; show_target; destroy ;;
    all)
      check_deps
      show_target
      preflight_scheduling
      install_storage
      install_operator
      smoke_test
      ;;
    *)
      echo "Usage: $0 [all|storage|operator|smoke|status|destroy]" >&2
      exit 1
      ;;
  esac
}

main "$@"
