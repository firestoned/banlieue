#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# OIDC through GitHub on an EXISTING k0s cluster — the k0s counterpart of
# scripts/dev-oidc-kind.sh (read that file's header first: why Dex is needed
# at all, and why the issuer is https://127.0.0.1:32000/dex).
#
# WHAT IS DIFFERENT ON k0s
#
#   - No kind port mapping. Dex is a NodePort, and kube-proxy (iptables mode,
#     localhost NodePorts on by default) answers 127.0.0.1:32000 on every
#     node — including the controllers, where kube-apiserver runs as a host
#     process. So the API server still reaches the issuer by the one URL.
#   - kube-apiserver is not a static pod. k0s starts it from
#     /etc/k0s/k0s.yaml (`spec.api.extraArgs`), and restarting the
#     `k0scontroller` unit is what applies a change. That is done ONE
#     controller at a time, each checked healthy with the flag actually in
#     the running process before the next is touched.
#   - Structured authentication (`--authentication-config`), not --oidc-*
#     flags. The CA travels inline in the file, so there is one file per
#     controller instead of a flag set plus a CA file, and kube-apiserver
#     re-reads it on change: only the first enablement restarts anything.
#     Both live under /etc/k0s, which Kairos keeps across reboots.
#   - A GitHub App, not an OAuth App: its client ID/secret come from
#     BANLIEUE_GITHUB_APP_CLIENT_ID / BANLIEUE_GITHUB_APP_CLIENT_SECRET. The
#     App needs the account permission "Email addresses: read-only" — Dex
#     reads /user/emails when a profile's email is private, and without that
#     permission the login fails after GitHub has already said yes.
#   - Nothing grants access to `system:authenticated`. Unlike a throwaway
#     kind cluster, this one is reachable by anyone who can reach its nodes,
#     and any GitHub account can finish the Dex login. `grant` binds the one
#     identity you logged in as.
#   - kubectl talks to Dex from the workstation through a host-side socat
#     proxy on 127.0.0.1:32000 (a container, so nothing to install). From a
#     browser on another machine, forward both ports over SSH:
#         ssh -L 8000:127.0.0.1:8000 -L 32000:127.0.0.1:32000 bar.foo.io
#
# Nothing here names a real host. Controllers are discovered from the
# cluster; the cluster comes from KUBECONFIG.
set -euo pipefail

NODE_PORT="${NODE_PORT:-32000}"
# How the issuer is reached (it must be ONE URL for the browser, kubelogin and
# every kube-apiserver, because it has to equal the token's `iss`):
#   local      https://127.0.0.1:NODE_PORT/dex via a socat proxy on this
#              machine. Private CA; a browser elsewhere needs SSH tunnels.
#   tailscale  https://<this machine's MagicDNS name>/dex via `tailscale
#              serve`, which terminates TLS with the tailnet's Let's Encrypt
#              certificate. No private CA to trust anywhere, and any tailnet
#              device (a laptop running kubectl) logs in with no tunnels.
#              The controllers must be on the tailnet to reach it.
EXPOSE="${EXPOSE:-local}"
if [[ -z "${ISSUER:-}" ]]; then
  if [[ "$EXPOSE" == "tailscale" ]]; then
    ISSUER="https://$(tailscale status --json \
      | python3 -c 'import json,sys; print(json.load(sys.stdin)["Self"]["DNSName"].rstrip("."))')/dex"
  else
    ISSUER="https://127.0.0.1:${NODE_PORT}/dex"
  fi
fi
# kubelogin's client. PUBLIC (PKCE, no secret): a secret shipped in a script
# authenticates nothing, and this cluster is not throwaway.
CLIENT_ID="${CLIENT_ID:-banlieue-dev}"
GITHUB_APP_CLIENT_ID="${BANLIEUE_GITHUB_APP_CLIENT_ID:-}"
GITHUB_APP_CLIENT_SECRET="${BANLIEUE_GITHUB_APP_CLIENT_SECRET:-}"
DEX_IMAGE="${DEX_IMAGE:-ghcr.io/dexidp/dex:v2.45.1}"
# `id` inside that image.
DEX_UID="${DEX_UID:-1001}"
# Kept, not /tmp: the CA signs the certificate every controller trusts.
WORK_DIR="${WORK_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/banlieue-oidc-k0s}"
NAMESPACE="${NAMESPACE:-banlieue-system}"
# root on bare metal; any other user must have passwordless sudo.
SSH_USER="${SSH_USER:-root}"
# Space-separated controller addresses. Empty: every control-plane node's
# InternalIP, from the cluster.
CONTROLLERS="${CONTROLLERS:-}"
# Prepended by kube-apiserver to every OIDC username and group. Named for the
# upstream identity provider, so `github:octocat` cannot be mistaken for, or
# collide with, any other authenticator's subject. Groups need it most: Dex's
# GitHub connector spells a team `org:team`, so unprefixed, a GitHub org called
# `system` could hand out `system:masters`. An empty PREFIX means the default,
# never no prefix.
PREFIX="${PREFIX:-github:}"
AUTHN_DIR="${AUTHN_DIR:-/etc/k0s/oidc}"
AUTHN_FILE="${AUTHN_DIR}/authentication-config.yaml"
# k0s runs kube-apiserver as this unprivileged user (installConfig.users.
# kubeAPIserverUser) with gid 0 and no supplementary groups, so the
# authentication config is owned by that user. Where the user does not exist
# (an API server running as root) the file stays root-owned.
APISERVER_USER="${APISERVER_USER:-kube-apiserver}"
K0S_CONFIG="${K0S_CONFIG:-/etc/k0s/k0s.yaml}"
K0S_BACKUP="${K0S_CONFIG}.pre-oidc"
PROXY_NAME="${PROXY_NAME:-banlieue-dex-proxy}"
# Fully qualified: podman (often behind `docker`) refuses short names.
SOCAT_IMAGE="${SOCAT_IMAGE:-docker.io/alpine/socat:latest}"
OIDC_USER="${OIDC_USER:-banlieue-oidc}"
APISERVER_PORT="${APISERVER_PORT:-6443}"
RESTART_TIMEOUT_SECS="${RESTART_TIMEOUT_SECS:-300}"
DISCOVERY_TIMEOUT_SECS="${DISCOVERY_TIMEOUT_SECS:-90}"

log()  { echo "==> $*" >&2; }
warn() { echo "!!! $*" >&2; }

check_deps() {
  local missing=()
  local tools=(kubectl openssl ssh nc)
  if [[ "$EXPOSE" == "tailscale" ]]; then tools+=(tailscale curl); else tools+=(docker); fi
  for c in "${tools[@]}"; do
    command -v "$c" >/dev/null 2>&1 || missing+=("$c")
  done
  [[ ${#missing[@]} -eq 0 ]] || { warn "missing: ${missing[*]}"; exit 1; }
  kubectl get --raw /readyz >/dev/null \
    || { warn "kubectl cannot reach the cluster (KUBECONFIG=${KUBECONFIG:-~/.kube/config})"; exit 1; }
}

require_github_app() {
  [[ -n "$GITHUB_APP_CLIENT_ID" && -n "$GITHUB_APP_CLIENT_SECRET" ]] && return 0
  warn "BANLIEUE_GITHUB_APP_CLIENT_ID / BANLIEUE_GITHUB_APP_CLIENT_SECRET are unset."
  warn "From a GitHub App (Settings → Developer settings → GitHub Apps):"
  warn "  Callback URL:  ${ISSUER}/callback"
  warn "  Account permissions → Email addresses: Read-only"
  exit 1
}

controllers() {
  if [[ -n "$CONTROLLERS" ]]; then echo "$CONTROLLERS"; return; fi
  kubectl get nodes -l node-role.kubernetes.io/control-plane \
    -o jsonpath='{range .items[*]}{.status.addresses[?(@.type=="InternalIP")].address}{" "}{end}'
}

# Run a script as root on a controller. The script travels base64-encoded so
# no remote shell's quoting rules apply, and stdin stays free for data.
on() {
  local host="$1"; shift
  local sudo="" script
  [[ "$SSH_USER" == "root" ]] || sudo="sudo -n"
  script="$(printf '%s' "$*" | base64 -w0)"
  ssh -o BatchMode=yes -o ConnectTimeout=10 "${SSH_USER}@${host}" \
    "echo $script | base64 -d > /tmp/.banlieue-oidc.\$\$ && $sudo sh /tmp/.banlieue-oidc.\$\$; rc=\$?; rm -f /tmp/.banlieue-oidc.\$\$; exit \$rc"
}

# ---------------------------------------------------------------------------
# TLS for Dex — same shape as the kind script: an IP SAN for 127.0.0.1,
# because that is the address the browser, kubectl and the API server use.
# ---------------------------------------------------------------------------

make_certs() {
  mkdir -p "$WORK_DIR"; chmod 700 "$WORK_DIR"
  if [[ -f "$WORK_DIR/ca.pem" && -f "$WORK_DIR/tls.crt" ]]; then
    log "certificates exist in $WORK_DIR, reusing"
    return 0
  fi
  log "generating a CA and a Dex serving certificate (IP SAN 127.0.0.1)"
  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "$WORK_DIR/ca-key.pem" -out "$WORK_DIR/ca.pem" \
    -subj "/CN=banlieue dev OIDC CA" 2>/dev/null
  openssl req -newkey rsa:2048 -nodes \
    -keyout "$WORK_DIR/tls.key" -out "$WORK_DIR/tls.csr" \
    -subj "/CN=127.0.0.1" 2>/dev/null
  printf 'subjectAltName = IP:127.0.0.1, DNS:localhost\nextendedKeyUsage = serverAuth\n' \
    >"$WORK_DIR/ext.cnf"
  openssl x509 -req -in "$WORK_DIR/tls.csr" \
    -CA "$WORK_DIR/ca.pem" -CAkey "$WORK_DIR/ca-key.pem" -CAcreateserial \
    -out "$WORK_DIR/tls.crt" -days 3650 -extfile "$WORK_DIR/ext.cnf" 2>/dev/null
  chmod 600 "$WORK_DIR/ca-key.pem" "$WORK_DIR/tls.key"
}

# ---------------------------------------------------------------------------
# Dex
# ---------------------------------------------------------------------------

deploy_dex() {
  require_github_app
  log "deploying Dex ($DEX_IMAGE) with the GitHub connector"
  kubectl create namespace dex --dry-run=client -o yaml | kubectl apply -f - >/dev/null
  kubectl -n dex create secret tls dex-tls \
    --cert="$WORK_DIR/tls.crt" --key="$WORK_DIR/tls.key" \
    --dry-run=client -o yaml | kubectl apply -f - >/dev/null
  kubectl -n dex create secret generic dex-github \
    --from-literal=client-id="$GITHUB_APP_CLIENT_ID" \
    --from-literal=client-secret="$GITHUB_APP_CLIENT_SECRET" \
    --dry-run=client -o yaml | kubectl apply -f - >/dev/null

  kubectl apply -f - <<EOF >/dev/null
apiVersion: v1
kind: ServiceAccount
metadata: { name: dex, namespace: dex }
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata: { name: dex }
rules:
  - apiGroups: ["dex.coreos.com"]
    resources: ["*"]
    verbs: ["*"]
  - apiGroups: ["apiextensions.k8s.io"]
    resources: ["customresourcedefinitions"]
    verbs: ["create", "get", "list", "watch"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata: { name: dex }
roleRef: { apiGroup: rbac.authorization.k8s.io, kind: ClusterRole, name: dex }
subjects:
  - { kind: ServiceAccount, name: dex, namespace: dex }
---
apiVersion: v1
kind: ConfigMap
metadata: { name: dex-config, namespace: dex }
data:
  config.yaml: |
    issuer: $ISSUER
    storage:
      # Signing keys in CRDs, so a pod restart does not rotate them and
      # invalidate every token (see the kind script for how that bit).
      type: kubernetes
      config:
        inCluster: true
    web:
      https: 0.0.0.0:5556
      tlsCert: /etc/dex/tls/tls.crt
      tlsKey: /etc/dex/tls/tls.key
    oauth2:
      skipApprovalScreen: true
    staticClients:
      - id: $CLIENT_ID
        name: banlieue dev
        public: true
        redirectURIs:
          - http://localhost:8000
          - http://localhost:18000
    connectors:
      - type: github
        id: github
        name: GitHub
        config:
          clientID: \$GITHUB_CLIENT_ID
          clientSecret: \$GITHUB_CLIENT_SECRET
          redirectURI: $ISSUER/callback
          loadAllGroups: false
---
apiVersion: apps/v1
kind: Deployment
metadata: { name: dex, namespace: dex }
spec:
  replicas: 1
  selector: { matchLabels: { app: dex } }
  template:
    metadata: { labels: { app: dex } }
    spec:
      serviceAccountName: dex
      securityContext:
        runAsNonRoot: true
        # The image's USER is the NAME "dex", which the kubelet cannot check
        # against runAsNonRoot; this is its numeric id.
        runAsUser: $DEX_UID
        runAsGroup: $DEX_UID
        seccompProfile: { type: RuntimeDefault }
      containers:
        - name: dex
          image: $DEX_IMAGE
          command: ["/usr/local/bin/dex", "serve", "/etc/dex/cfg/config.yaml"]
          ports: [{ containerPort: 5556 }]
          env:
            - name: GITHUB_CLIENT_ID
              valueFrom: { secretKeyRef: { name: dex-github, key: client-id } }
            - name: GITHUB_CLIENT_SECRET
              valueFrom: { secretKeyRef: { name: dex-github, key: client-secret } }
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities: { drop: ["ALL"] }
          volumeMounts:
            - { name: config, mountPath: /etc/dex/cfg }
            - { name: tls, mountPath: /etc/dex/tls }
      volumes:
        - { name: config, configMap: { name: dex-config } }
        - { name: tls, secret: { secretName: dex-tls } }
---
apiVersion: v1
kind: Service
metadata: { name: dex, namespace: dex }
spec:
  type: NodePort
  selector: { app: dex }
  ports:
    - port: 5556
      targetPort: 5556
      nodePort: $NODE_PORT
EOF
  # A config or credential change must reach the running pod.
  kubectl -n dex rollout restart deploy/dex >/dev/null
  kubectl -n dex rollout status deploy/dex --timeout=180s
}

# The check that matters: can each CONTROLLER fetch and trust the issuer by
# the exact URL the API server will use? Done before any controller is
# touched, so a broken Dex never costs a control-plane restart.
verify_discovery() {
  local host
  for host in $(controllers); do
    local deadline=$((SECONDS + DISCOVERY_TIMEOUT_SECS)) ok=false
    while (( SECONDS <= deadline )); do
      # tailscale: a public certificate, so the node's own trust store —
      # which is what kube-apiserver uses when the config names no CA.
      local cacert=""
      [[ "$EXPOSE" == "tailscale" ]] || cacert="--cacert /tmp/banlieue-oidc-ca.pem"
      if on "$host" "cat >/tmp/banlieue-oidc-ca.pem && \
          curl -fsS $cacert ${ISSUER}/.well-known/openid-configuration >/dev/null; \
          rc=\$?; rm -f /tmp/banlieue-oidc-ca.pem; exit \$rc" <"$WORK_DIR/ca.pem" 2>/dev/null; then
        ok=true; break
      fi
      sleep 3
    done
    if [[ "$ok" != "true" ]]; then
      warn "$host cannot fetch and trust ${ISSUER}/.well-known/openid-configuration"
      warn "  Until it can, its API server would reject every OIDC token."
      exit 1
    fi
    log "  ✓ $host fetches and trusts $ISSUER"
  done
}

# ---------------------------------------------------------------------------
# kube-apiserver: structured authentication, one controller at a time
# ---------------------------------------------------------------------------

# The private CA is inlined only when the issuer serves the dev certificate;
# a tailscale issuer has a public one, and kube-apiserver then uses the
# system trust store.
authn_ca() {
  [[ "$EXPOSE" == "tailscale" ]] && return 0
  echo "      certificateAuthority: |"
  sed 's/^/        /' "$WORK_DIR/ca.pem"
}

authn_config() {
  cat <<EOF
apiVersion: apiserver.config.k8s.io/v1
kind: AuthenticationConfiguration
jwt:
  - issuer:
      url: $ISSUER
      audiences: ["$CLIENT_ID"]
$(authn_ca)
    claimMappings:
      # Readable identities (${PREFIX}<github-login>). ADR-0047's subject
      # policy must carry the same value as its usernamePrefix param.
      username: { claim: preferred_username, prefix: "${PREFIX}" }
      groups: { claim: groups, prefix: "${PREFIX}" }
EOF
}

# How many --authentication-config flags the RUNNING kube-apiserver has.
# Read from /proc by comm, so the probe never matches itself.
running_flag() {
  on "$1" 'for p in /proc/[0-9]*; do
      [ "$(cat $p/comm 2>/dev/null)" = kube-apiserver ] || continue
      tr "\0" "\n" <$p/cmdline; done | grep -c "^--authentication-config=" || true'
}

apiserver_ready() {
  kubectl --server "https://$1:${APISERVER_PORT}" get --raw /readyz >/dev/null 2>&1
}

attach_controller() {
  local host="$1"
  log "controller $host"
  # Written aside and renamed into place: kube-apiserver watches this file,
  # and writing it in place let it read a half-written file (one failed
  # reload per controller, observed in its reload metrics). Owned
  # APISERVER_USER:root, 0750/0640: kube-apiserver is not root under k0s, and
  # a root-only file fails it at startup with "permission denied". Its group
  # is 0, not kube-apiserver, so group ownership alone does not help.
  authn_config | on "$host" "u=$APISERVER_USER; id -u \$u >/dev/null 2>&1 || u=root; \
    install -d -m 0750 $AUTHN_DIR && chown \$u:root $AUTHN_DIR && chmod 0750 $AUTHN_DIR && \
    cat >$AUTHN_FILE.tmp && chown \$u:root $AUTHN_FILE.tmp && chmod 0640 $AUTHN_FILE.tmp && \
    mv -f $AUTHN_FILE.tmp $AUTHN_FILE"

  if on "$host" "grep -q 'authentication-config:' $K0S_CONFIG"; then
    log "  $K0S_CONFIG already names the file; the API server reloads it on change"
  else
    if on "$host" "grep -q '^    extraArgs:' $K0S_CONFIG"; then
      warn "  $K0S_CONFIG already has spec.api.extraArgs; add by hand:"
      warn "    authentication-config: $AUTHN_FILE"
      exit 1
    fi
    # awk, not `sed -i` with \n: Kairos ships a non-GNU sed. `cat >` keeps
    # the file's inode and mode.
    on "$host" "cp -p $K0S_CONFIG $K0S_BACKUP && \
      awk -v f='$AUTHN_FILE' '{print} /^  api:\$/ {print \"    extraArgs:\"; print \"      authentication-config: \" f}' \
        $K0S_BACKUP >$K0S_CONFIG.tmp && cat $K0S_CONFIG.tmp >$K0S_CONFIG && rm -f $K0S_CONFIG.tmp && \
      grep -q 'authentication-config: $AUTHN_FILE' $K0S_CONFIG"
    log "  restarting k0scontroller"
    on "$host" "systemctl restart k0scontroller"
  fi

  local deadline=$((SECONDS + RESTART_TIMEOUT_SECS))
  # /readyz alone passes for an API server that came back WITHOUT the flag,
  # so the running process is checked too.
  until apiserver_ready "$host" && [[ "$(running_flag "$host")" -ge 1 ]]; do
    if (( SECONDS > deadline )); then
      warn "  $host did not come back with the flag in ${RESTART_TIMEOUT_SECS}s; restoring"
      on "$host" "[ -f $K0S_BACKUP ] && cp -p $K0S_BACKUP $K0S_CONFIG && systemctl restart k0scontroller" || true
      exit 1
    fi
    sleep 5
  done
  log "  ✓ $host ready, --authentication-config in effect"
}

attach_all() {
  local host
  for host in $(controllers); do attach_controller "$host"; done
}

detach_all() {
  local host
  for host in $(controllers); do
    log "controller $host"
    if on "$host" "[ -f $K0S_BACKUP ]"; then
      on "$host" "cp -p $K0S_BACKUP $K0S_CONFIG && rm -f $K0S_BACKUP && systemctl restart k0scontroller"
      local deadline=$((SECONDS + RESTART_TIMEOUT_SECS))
      until apiserver_ready "$host" && [[ "$(running_flag "$host")" -eq 0 ]]; do
        (( SECONDS <= deadline )) || { warn "  $host not ready after restore"; exit 1; }
        sleep 5
      done
    fi
    on "$host" "rm -rf $AUTHN_DIR"
    log "  ✓ $host restored"
  done
}

# ---------------------------------------------------------------------------
# Workstation side
# ---------------------------------------------------------------------------

# 127.0.0.1:NODE_PORT on this machine → a controller's NodePort. Targets the
# NodePort, not a pod, so it survives Dex restarts (see the kind script).
start_proxy() {
  local target; target="$(controllers | awk '{print $1}')"
  [[ -n "$target" ]] || { warn "no controller address"; exit 1; }
  docker rm -f "$PROXY_NAME" >/dev/null 2>&1 || true
  log "proxy 127.0.0.1:$NODE_PORT -> ${target}:${NODE_PORT}"
  docker run -d --name "$PROXY_NAME" --network host --restart unless-stopped \
    "$SOCAT_IMAGE" \
    "tcp-listen:${NODE_PORT},bind=127.0.0.1,fork,reuseaddr" \
    "tcp-connect:${target}:${NODE_PORT}" >/dev/null
  local deadline=$((SECONDS + 30))
  until nc -z 127.0.0.1 "$NODE_PORT" 2>/dev/null; do
    (( SECONDS <= deadline )) || { warn "proxy never listened"; docker logs "$PROXY_NAME" 2>&1 | tail -5 >&2; exit 1; }
    sleep 1
  done
  log "  ✓ 127.0.0.1:$NODE_PORT reaches Dex"
}

# https://<MagicDNS name>:443 on the tailnet → a controller's NodePort.
# tailscale terminates TLS with its own certificate for the name; the hop to
# Dex is the libvirt/host network and carries Dex's dev certificate, hence
# https+insecure for that leg only.
start_serve() {
  local target; target="$(controllers | awk '{print $1}')"
  [[ -n "$target" ]] || { warn "no controller address"; exit 1; }
  docker rm -f "$PROXY_NAME" >/dev/null 2>&1 || true
  log "tailscale serve https:443 -> ${target}:${NODE_PORT}"
  tailscale serve --bg --https=443 "https+insecure://${target}:${NODE_PORT}" >/dev/null \
    || { warn "tailscale serve failed; as root once: tailscale set --operator=\$USER"; exit 1; }
  local deadline=$((SECONDS + DISCOVERY_TIMEOUT_SECS))
  until curl -fsS "${ISSUER}/.well-known/openid-configuration" >/dev/null 2>&1; do
    (( SECONDS <= deadline )) || { warn "${ISSUER} never answered"; exit 1; }
    sleep 3
  done
  log "  ✓ ${ISSUER} reaches Dex with a publicly trusted certificate"
}

expose() {
  if [[ "$EXPOSE" == "tailscale" ]]; then start_serve; else start_proxy; fi
}

# A kubeconfig for a workstation that logs in through Dex: the cluster's
# server and CA, and an exec user running kubelogin. It carries NO admin
# credential, so it is safe to copy to a laptop.
print_kubeconfig() {
  local tmp; tmp="$(mktemp -d)"
  local server cluster
  server="$(kubectl config view --minify -o jsonpath='{.clusters[0].cluster.server}')"
  cluster="$(kubectl config view --minify -o jsonpath='{.contexts[0].context.cluster}')"
  kubectl config view --minify --raw \
    -o jsonpath='{.clusters[0].cluster.certificate-authority-data}' | base64 -d >"$tmp/ca.crt"
  local kc=(kubectl config --kubeconfig "$tmp/config")
  "${kc[@]}" set-cluster "$cluster" --server="$server" \
    --certificate-authority="$tmp/ca.crt" --embed-certs=true >/dev/null
  local ca_arg=()
  [[ "$EXPOSE" == "tailscale" ]] || ca_arg=(--exec-arg="--certificate-authority=$WORK_DIR/ca.pem")
  "${kc[@]}" set-credentials "$OIDC_USER" \
    --exec-api-version=client.authentication.k8s.io/v1 \
    --exec-interactive-mode=IfAvailable \
    --exec-command=kubectl \
    --exec-arg=oidc-login --exec-arg=get-token \
    --exec-arg="--oidc-issuer-url=$ISSUER" \
    --exec-arg="--oidc-client-id=$CLIENT_ID" \
    --exec-arg="--oidc-extra-scope=profile" \
    --exec-arg="--oidc-extra-scope=email" \
    --exec-arg="--oidc-extra-scope=groups" \
    ${ca_arg[@]+"${ca_arg[@]}"} >/dev/null
  "${kc[@]}" set-context "$OIDC_USER" --cluster="$cluster" \
    --user="$OIDC_USER" --namespace="$NAMESPACE" >/dev/null
  "${kc[@]}" use-context "$OIDC_USER" >/dev/null
  cat "$tmp/config"
  rm -rf "$tmp"
}

setup_login() {
  command -v kubectl-oidc_login >/dev/null 2>&1 || {
    warn "kubelogin is not installed: kubectl krew install oidc-login, or put the"
    warn "binary from https://github.com/int128/kubelogin/releases on PATH as"
    warn "kubectl-oidc_login"
    exit 1; }
  local cluster; cluster="$(kubectl config view --minify -o jsonpath='{.contexts[0].context.cluster}')"
  local ca_arg=()
  [[ "$EXPOSE" == "tailscale" ]] || ca_arg=(--exec-arg="--certificate-authority=$WORK_DIR/ca.pem")
  # One --exec-arg per scope; a comma list is split into bare args (see kind).
  kubectl config set-credentials "$OIDC_USER" \
    --exec-api-version=client.authentication.k8s.io/v1 \
    --exec-interactive-mode=IfAvailable \
    --exec-command=kubectl \
    --exec-arg=oidc-login --exec-arg=get-token \
    --exec-arg="--oidc-issuer-url=$ISSUER" \
    --exec-arg="--oidc-client-id=$CLIENT_ID" \
    --exec-arg="--oidc-extra-scope=profile" \
    --exec-arg="--oidc-extra-scope=email" \
    --exec-arg="--oidc-extra-scope=groups" \
    ${ca_arg[@]+"${ca_arg[@]}"} >/dev/null
  kubectl config set-context "$OIDC_USER" --cluster="$cluster" \
    --user="$OIDC_USER" --namespace="$NAMESPACE" >/dev/null
  if [[ "$EXPOSE" != "tailscale" ]]; then
    log "context '$OIDC_USER' created. Remote browser? Forward first:"
    log "  ssh -L 8000:127.0.0.1:8000 -L ${NODE_PORT}:127.0.0.1:${NODE_PORT} <this host>"
  fi
  kubectl --context "$OIDC_USER" auth whoami
}

# Claim access for the identity you just logged in as — and nobody else.
grant_me() {
  # GRANT_USER=${PREFIX}<login> when you logged in from another machine.
  local me="${GRANT_USER:-}"
  [[ -n "$me" ]] || me="$(kubectl --context "$OIDC_USER" auth whoami \
    -o jsonpath='{.status.userInfo.username}' 2>/dev/null || true)"
  [[ "$me" == "$PREFIX"* ]] || { warn "no OIDC identity (got '$me'): run login, or set GRANT_USER=${PREFIX}<login>"; exit 1; }
  log "granting claim access in $NAMESPACE to $me"
  kubectl apply -f - <<EOF >/dev/null
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata: { name: banlieue-claim-author, namespace: $NAMESPACE }
rules:
  - apiGroups: ["banlieue.io"]
    resources: ["virtualmachineclaims"]
    verbs: ["get", "list", "watch", "create", "delete"]
  - apiGroups: ["banlieue.io"]
    resources: ["virtualmachinepools", "virtualmachines"]
    verbs: ["get", "list", "watch"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata: { name: banlieue-claim-author-oidc, namespace: $NAMESPACE }
roleRef: { apiGroup: rbac.authorization.k8s.io, kind: Role, name: banlieue-claim-author }
subjects:
  - { kind: User, name: "$me", apiGroup: rbac.authorization.k8s.io }
EOF
}

status() {
  echo "issuer:      $ISSUER"
  echo "work dir:    $WORK_DIR"
  echo "controllers: $(controllers)"
  local host
  for host in $(controllers); do
    echo "  $host: authentication-config flags running = $(running_flag "$host" 2>/dev/null || echo '?')"
  done
  echo "expose:      $EXPOSE"
  if [[ "$EXPOSE" == "tailscale" ]]; then
    echo -n "issuer:      "; (curl -fsS "${ISSUER}/.well-known/openid-configuration" >/dev/null 2>&1 && echo "answering") || echo DOWN
  else
    echo -n "proxy:       "; (nc -z 127.0.0.1 "$NODE_PORT" 2>/dev/null && echo "127.0.0.1:$NODE_PORT open") || echo DOWN
  fi
  kubectl -n dex get pods 2>/dev/null || true
  kubectl --context "$OIDC_USER" auth whoami 2>/dev/null || echo "not logged in (run: $0 login)"
}

teardown() {
  detach_all
  log "removing Dex, the proxy and the kubectl context"
  kubectl delete namespace dex --ignore-not-found >/dev/null
  kubectl delete clusterrolebinding dex --ignore-not-found >/dev/null
  kubectl delete clusterrole dex --ignore-not-found >/dev/null
  kubectl -n "$NAMESPACE" delete rolebinding banlieue-claim-author-oidc --ignore-not-found >/dev/null
  docker rm -f "$PROXY_NAME" >/dev/null 2>&1 || true
  [[ "$EXPOSE" != "tailscale" ]] || tailscale serve --https=443 off >/dev/null 2>&1 || true
  kubectl config delete-context "$OIDC_USER" >/dev/null 2>&1 || true
  kubectl config delete-user "$OIDC_USER" >/dev/null 2>&1 || true
  rm -rf "$WORK_DIR"
  log "clean"
}

main() {
  case "${1:-}" in
    up)
      check_deps; require_github_app
      make_certs; deploy_dex; expose; verify_discovery; attach_all
      log "Ready. Now: $0 login, then $0 grant"
      ;;
    certs)  check_deps; make_certs ;;
    dex)    check_deps; make_certs; deploy_dex; verify_discovery ;;
    verify) check_deps; verify_discovery ;;
    attach) check_deps; verify_discovery; attach_all ;;
    expose) check_deps; expose ;;
    kubeconfig) print_kubeconfig ;;
    login)  check_deps; setup_login ;;
    grant)  check_deps; grant_me ;;
    status) status ;;
    down)   check_deps; teardown ;;
    *)
      cat >&2 <<EOF
Usage: $0 [up|login|kubeconfig|grant|status|down|certs|dex|expose|verify|attach]

  up          Dex, exposed (EXPOSE=local|tailscale), discovery check from every
              controller, then the kube-apiserver change one controller at a time.
              Re-running with a new issuer rewrites the file; no restart.
  kubeconfig  print a kubeconfig for another machine (no admin credential)
  login   kubectl context '$OIDC_USER' through kubelogin; prints who you are
  grant   claim access in $NAMESPACE for the identity you logged in as
  status  what exists, per controller, and who you are
  down    restore every controller, remove Dex, the proxy and the CA

Env: KUBECONFIG, SSH_USER (default root; else passwordless sudo),
     CONTROLLERS (default: discovered), BANLIEUE_GITHUB_APP_CLIENT_ID/SECRET,
     EXPOSE (local | tailscale), PREFIX (default github:),
     GRANT_USER (${PREFIX}<login>).
GitHub App callback URL: ${ISSUER}/callback
EOF
      exit 1
      ;;
  esac
}

main "$@"
