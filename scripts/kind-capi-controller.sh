#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Run banlieue-controller as a LOCAL binary against the CAPI e2e kind cluster,
# under the banlieue-controller ServiceAccount's own identity (ADR-0129 e2e).
#
#   scripts/kind-capi-controller.sh start   # mint a token, write a kubeconfig, start
#   scripts/kind-capi-controller.sh stop    # stop it; print its log tail with LOG_TAIL=N
#
# Why a local binary: the CAPI e2e cluster's banlieue components are rendered
# with an image tag that is never loaded, and images are not built here. A
# kind cluster plus the controller as a local binary is the repo's documented
# way to run an e2e without one (rules/testing.md).
#
# Why the ServiceAccount's identity, not the admin kubeconfig: a test with
# more privilege than production hides bugs (rules/testing.md, rule 3). The
# controller must read bootstrap Secrets only where a Role lets it, and the
# e2e asserts exactly that. `start` checks the identity before starting.
#
# Environment:
#   CAPI_KIND_KUBECONFIG  admin kubeconfig of the kind cluster (required by start)
#   BANLIEUE_BIN          the banlieue binary (default target/debug/banlieue)
#   STATE_DIR             kubeconfig, pid and log (default target/kind-capi-controller)
#   CONTROLLER_NAMESPACE  where the controller's ServiceAccount lives (default banlieue-system)
set -euo pipefail

readonly SA_NAME=banlieue-controller
readonly TOKEN_DURATION=2h
# Ports for the local health and metrics servers, away from the defaults so
# a controller already running on this machine does not collide.
readonly HEALTH_PORT=18081
readonly METRICS_PORT=18080
# Seconds to wait for the controller to report its CAPI reconcilers started.
readonly START_TIMEOUT_S=60
readonly STARTED_LINE="starting CAPI bootstrap-data controllers"

BIN="${BANLIEUE_BIN:-target/debug/banlieue}"
STATE_DIR="${STATE_DIR:-target/kind-capi-controller}"
NS="${CONTROLLER_NAMESPACE:-banlieue-system}"
SA_KUBECONFIG="$STATE_DIR/kubeconfig"
PID_FILE="$STATE_DIR/pid"
LOG_FILE="$STATE_DIR/controller.log"

log() { echo "==> $*" >&2; }

# can_i <expected yes|no> <verb> <resource> [namespace flags...]
can_i() {
  local want="$1"; shift
  local got
  got="$(kubectl --kubeconfig "$SA_KUBECONFIG" auth can-i "$@" 2>/dev/null || true)"
  if [[ "$got" != "$want" ]]; then
    log "the controller's identity answers '$got' to 'can-i $*', expected '$want'"
    exit 1
  fi
}

start() {
  local admin="${CAPI_KIND_KUBECONFIG:?set CAPI_KIND_KUBECONFIG to the admin kubeconfig of the kind cluster}"
  [[ -x "$BIN" ]] || { log "$BIN not found; build it with: cargo build -p banlieue"; exit 1; }
  stop_quietly
  mkdir -p "$STATE_DIR"
  local server ca token
  server="$(kubectl --kubeconfig "$admin" config view --minify -o jsonpath='{.clusters[0].cluster.server}')"
  ca="$(kubectl --kubeconfig "$admin" config view --minify --raw -o jsonpath='{.clusters[0].cluster.certificate-authority-data}')"
  token="$(kubectl --kubeconfig "$admin" -n "$NS" create token "$SA_NAME" --duration "$TOKEN_DURATION")"
  ( umask 077
    cat >"$SA_KUBECONFIG" <<EOF
apiVersion: v1
kind: Config
clusters:
  - name: kind
    cluster:
      server: ${server}
      certificate-authority-data: ${ca}
users:
  - name: ${SA_NAME}
    user:
      token: ${token}
contexts:
  - name: ${SA_NAME}
    context: { cluster: kind, user: ${SA_NAME} }
current-context: ${SA_NAME}
EOF
  )
  # The production identity: reads Machines everywhere, Secrets only in its
  # own namespace, nowhere cluster-wide.
  can_i yes list machines.cluster.x-k8s.io --all-namespaces
  can_i yes get secrets -n "$NS"
  can_i no get secrets --all-namespaces
  log "starting $BIN controller as system:serviceaccount:$NS:$SA_NAME"
  "$BIN" controller --kubeconfig "$SA_KUBECONFIG" --no-leader-elect \
    --health-port "$HEALTH_PORT" --metrics-port "$METRICS_PORT" \
    >"$LOG_FILE" 2>&1 &
  echo $! >"$PID_FILE"
  local waited=0
  until grep -q "$STARTED_LINE" "$LOG_FILE"; do
    if ! kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
      log "the controller exited; its log:"; cat "$LOG_FILE" >&2; exit 1
    fi
    if (( waited >= START_TIMEOUT_S )); then
      log "no '$STARTED_LINE' after ${START_TIMEOUT_S}s; its log:"; cat "$LOG_FILE" >&2; exit 1
    fi
    sleep 1; waited=$((waited + 1))
  done
  log "controller running (pid $(cat "$PID_FILE")), log: $LOG_FILE"
}

stop_quietly() {
  [[ -f "$PID_FILE" ]] || return 0
  kill "$(cat "$PID_FILE")" 2>/dev/null || true
  rm -f "$PID_FILE"
}

stop() {
  stop_quietly
  if [[ -n "${LOG_TAIL:-}" && -f "$LOG_FILE" ]]; then
    log "controller log (last $LOG_TAIL lines):"
    tail -n "$LOG_TAIL" "$LOG_FILE" >&2
  fi
}

case "${1:-}" in
  start) start ;;
  stop) stop ;;
  *) echo "usage: $0 start|stop" >&2; exit 2 ;;
esac
