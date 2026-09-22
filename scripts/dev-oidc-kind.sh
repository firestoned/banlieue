#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# A dev cluster that authenticates you with your REAL GitHub account, so the
# VirtualMachineClaim subject policy can be exercised against a real identity
# instead of `kubernetes-admin` (ADR-0047 Decision 10).
#
# WHY DEX IS IN THE PICTURE
#
# GitHub is an OAuth2 provider but NOT an OIDC provider: it issues no ID
# token and serves no /.well-known/openid-configuration. `kube-apiserver
# --oidc-issuer-url` needs both, so it cannot point at GitHub. Dex bridges
# the two — it authenticates you against GitHub and mints a real OIDC ID
# token that the API server will accept.
#
# THE TOPOLOGY, AND THE ONE TRICK IN IT
#
#   browser ──┐
#             ├─▶ https://127.0.0.1:32000/dex  ──▶ GitHub
#   kubectl ──┘            ▲
#                          │  same URL, from inside the node
#   kube-apiserver ────────┘
#
# Dex runs in-cluster on NodePort 32000, and kind maps host port 32000 to the
# node. That makes ONE issuer URL — https://127.0.0.1:32000/dex — reachable
# from the browser, from kubectl on the host, and from the API server inside
# the node. An OIDC issuer URL must match the `iss` claim exactly, so having
# one spelling for all three is what keeps this simple. The serving
# certificate therefore needs an IP SAN for 127.0.0.1, not a DNS name.
#
# THE DETAIL THAT IS EASY TO MISS
#
# `--oidc-ca-file` needs the CA in TWO places, because there are two
# boundaries:
#   1. host  → node:            kind `extraMounts`
#   2. node  → apiserver pod:   kubeadm `apiServer.extraVolumes`
# The API server is a static pod and only sees host paths that kubeadm was
# told to mount. Setting just the kind mount yields "no such file or
# directory" from a component you cannot easily strace.
set -euo pipefail

CLUSTER="${CLUSTER:-banlieue-oidc}"
NODE_PORT="${NODE_PORT:-32000}"
ISSUER="${ISSUER:-https://127.0.0.1:${NODE_PORT}/dex}"
CLIENT_ID="${CLIENT_ID:-banlieue-dev}"
# Public client secret: this is a LOCAL DEV cluster and the secret is in this
# repo's docs. It authenticates nothing of value. Never reuse the pattern for
# anything reachable.
CLIENT_SECRET="${CLIENT_SECRET:-banlieue-dev-not-a-secret}"
WORK_DIR="${WORK_DIR:-${TMPDIR:-/tmp}/banlieue-oidc}"
NAMESPACE="${NAMESPACE:-banlieue-system}"

log()  { echo "==> $*" >&2; }
warn() { echo "!!! $*" >&2; }

check_deps() {
  local missing=()
  for c in kind kubectl openssl; do
    command -v "$c" >/dev/null 2>&1 || missing+=("$c")
  done
  [[ ${#missing[@]} -eq 0 ]] || { warn "missing: ${missing[*]}"; exit 1; }
}

require_github_app() {
  [[ -n "${GITHUB_CLIENT_ID:-}" && -n "${GITHUB_CLIENT_SECRET:-}" ]] && return 0
  warn "GITHUB_CLIENT_ID and GITHUB_CLIENT_SECRET are unset."
  warn ""
  warn "Create a GitHub OAuth App (not a GitHub App):"
  warn "  https://github.com/settings/developers  →  New OAuth App"
  warn "    Homepage URL:               ${ISSUER}"
  warn "    Authorization callback URL: ${ISSUER}/callback"
  warn ""
  warn "Then export its credentials and re-run:"
  warn "  export GITHUB_CLIENT_ID=...  GITHUB_CLIENT_SECRET=..."
  exit 1
}

# ---------------------------------------------------------------------------
# TLS for Dex
# ---------------------------------------------------------------------------

# A CA and a serving certificate with an IP SAN for 127.0.0.1.
#
# An IP SAN rather than a DNS name because that is the address all three
# consumers use; a certificate naming `dex.dex.svc` would satisfy the API
# server and fail in the browser, which is a confusing way to spend an hour.
make_certs() {
  mkdir -p "$WORK_DIR"
  if [[ -f "$WORK_DIR/ca.pem" && -f "$WORK_DIR/tls.crt" ]]; then
    log "certificates exist in $WORK_DIR, reusing (rm -rf it to regenerate)"
    return 0
  fi
  log "generating a CA and a Dex serving certificate (IP SAN 127.0.0.1)"

  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "$WORK_DIR/ca-key.pem" -out "$WORK_DIR/ca.pem" \
    -subj "/CN=banlieue dev OIDC CA" 2>/dev/null

  openssl req -newkey rsa:2048 -nodes \
    -keyout "$WORK_DIR/tls.key" -out "$WORK_DIR/tls.csr" \
    -subj "/CN=127.0.0.1" 2>/dev/null

  cat >"$WORK_DIR/ext.cnf" <<EOF
subjectAltName = IP:127.0.0.1, DNS:localhost
extendedKeyUsage = serverAuth
EOF

  openssl x509 -req -in "$WORK_DIR/tls.csr" \
    -CA "$WORK_DIR/ca.pem" -CAkey "$WORK_DIR/ca-key.pem" -CAcreateserial \
    -out "$WORK_DIR/tls.crt" -days 3650 \
    -extfile "$WORK_DIR/ext.cnf" 2>/dev/null

  chmod 600 "$WORK_DIR"/*key*.pem "$WORK_DIR/tls.key"
  log "  CA: $WORK_DIR/ca.pem"
}

# ---------------------------------------------------------------------------
# Cluster
# ---------------------------------------------------------------------------

create_cluster() {
  if kind get clusters 2>/dev/null | grep -qx "$CLUSTER"; then
    log "cluster $CLUSTER exists, keeping"
    return 0
  fi
  log "creating kind cluster $CLUSTER with OIDC pointed at $ISSUER"

  cat >"$WORK_DIR/kind.yaml" <<EOF
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
  - role: control-plane
    # 1. host -> node. Without this the node has no CA file at all.
    extraMounts:
      - hostPath: $WORK_DIR/ca.pem
        containerPath: /etc/oidc/ca.pem
        readOnly: true
    # Dex's NodePort, so one URL works from the browser and from in-cluster.
    extraPortMappings:
      - containerPort: $NODE_PORT
        hostPort: $NODE_PORT
        listenAddress: "127.0.0.1"
        protocol: TCP
    kubeadmConfigPatches:
      - |
        kind: ClusterConfiguration
        apiServer:
          extraArgs:
            oidc-issuer-url: $ISSUER
            oidc-client-id: $CLIENT_ID
            oidc-ca-file: /etc/oidc/ca.pem
            # preferred_username gives a readable identity. Dex populates it
            # from the GitHub login, so usernames look like oidc:octocat
            # rather than an opaque base64 subject — which matters here,
            # because spec.subject.id has to be typed by a human.
            oidc-username-claim: preferred_username
            oidc-username-prefix: "oidc:"
            oidc-groups-claim: groups
            oidc-groups-prefix: "oidc:"
          # 2. node -> apiserver static pod. The kind mount above is not
          #    enough on its own; the API server only sees what kubeadm
          #    mounts into it.
          extraVolumes:
            - name: oidc-ca
              hostPath: /etc/oidc/ca.pem
              mountPath: /etc/oidc/ca.pem
              readOnly: true
              pathType: File
EOF

  kind create cluster --name "$CLUSTER" --config "$WORK_DIR/kind.yaml" --wait 120s
}

# ---------------------------------------------------------------------------
# Dex
# ---------------------------------------------------------------------------

deploy_dex() {
  require_github_app
  log "deploying Dex with the GitHub connector"
  local kc=(kubectl --context "kind-${CLUSTER}")

  "${kc[@]}" create namespace dex --dry-run=client -o yaml | "${kc[@]}" apply -f - >/dev/null
  "${kc[@]}" -n dex create secret tls dex-tls \
    --cert="$WORK_DIR/tls.crt" --key="$WORK_DIR/tls.key" \
    --dry-run=client -o yaml | "${kc[@]}" apply -f - >/dev/null
  "${kc[@]}" -n dex create secret generic dex-github \
    --from-literal=client-id="$GITHUB_CLIENT_ID" \
    --from-literal=client-secret="$GITHUB_CLIENT_SECRET" \
    --dry-run=client -o yaml | "${kc[@]}" apply -f - >/dev/null

  "${kc[@]}" apply -f - <<EOF >/dev/null
apiVersion: v1
kind: ServiceAccount
metadata: { name: dex, namespace: dex }
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata: { name: dex }
rules:
  # Dex stores signing keys, auth codes and refresh tokens as its own CRs.
  - apiGroups: ["dex.coreos.com"]
    resources: ["*"]
    verbs: ["*"]
  # And creates those CRDs on first start.
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
      # NOT type memory, which was the first attempt and is wrong here: Dex
      # generates its token-signing keys at startup and keeps them in
      # memory, so every restart ROTATES them and invalidates every
      # outstanding ID token. The github-creds step restarts Dex by design,
      # so the documented sequence invalidated the login it had just
      # produced -- surfacing as Unauthorized / "invalid bearer token" from
      # the API server, hours after a login that genuinely worked.
      #
      # The kubernetes backend keeps keys and refresh tokens in CRDs, so a
      # pod replacement is invisible to anyone already logged in.
      type: kubernetes
      config:
        inCluster: true
    web:
      https: 0.0.0.0:5556
      tlsCert: /etc/dex/tls/tls.crt
      tlsKey: /etc/dex/tls/tls.key
    oauth2:
      # Skip the "grant access?" screen: this is a dev loop, and the consent
      # you care about is GitHub's own.
      skipApprovalScreen: true
    staticClients:
      - id: $CLIENT_ID
        name: banlieue dev
        secret: $CLIENT_SECRET
        redirectURIs:
          # kubelogin's local listener. The ports it may use are fixed, so
          # both are registered.
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
          # No org filter: any GitHub account may log in to this dev cluster.
          # Authorization is RBAC's job below, not the connector's.
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
      containers:
        - name: dex
          image: ghcr.io/dexidp/dex:v2.41.1
          command: ["/usr/local/bin/dex", "serve", "/etc/dex/cfg/config.yaml"]
          ports: [{ containerPort: 5556 }]
          env:
            - name: GITHUB_CLIENT_ID
              valueFrom: { secretKeyRef: { name: dex-github, key: client-id } }
            - name: GITHUB_CLIENT_SECRET
              valueFrom: { secretKeyRef: { name: dex-github, key: client-secret } }
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

  log "waiting for Dex"
  "${kc[@]}" -n dex rollout status deploy/dex --timeout=120s
}

# The check that actually matters: can the API SERVER reach the issuer and
# trust its certificate? A browser that works proves nothing about the
# component doing the token validation.
verify_discovery() {
  local kc=(kubectl --context "kind-${CLUSTER}")
  log "verifying OIDC discovery from inside the control-plane node"
  # Retry rather than judging on one attempt: right after a rollout the Dex
  # pod is Running before the Service has endpoints for it, so a single probe
  # reports a broken cluster that is merely a second from working. Observed.
  local deadline=$((SECONDS + 60)) ok=false
  while (( SECONDS <= deadline )); do
    if docker exec "${CLUSTER}-control-plane" \
        curl -sS --cacert /etc/oidc/ca.pem "${ISSUER}/.well-known/openid-configuration" \
        >/dev/null 2>&1; then
      ok=true; break
    fi
    sleep 2
  done
  if [[ "$ok" == "true" ]]; then
    log "  ✓ the API server's node can fetch and trust ${ISSUER}"
  else
    warn "  ✗ the node could NOT fetch ${ISSUER}/.well-known/openid-configuration"
    warn "    Until this works the API server will reject every OIDC token."
    warn "    Check: the Dex pod is Running, the NodePort is $NODE_PORT, and"
    warn "    the certificate carries an IP SAN for 127.0.0.1."
    exit 1
  fi
}

# ---------------------------------------------------------------------------
# Attach OIDC to an EXISTING kind cluster
# ---------------------------------------------------------------------------
#
# `up` bakes the OIDC flags in at creation, which is the clean way. But a
# cluster that already has a Provider, a warm pool and real VMs on a
# hypervisor is worth more than a clean one, and you cannot re-create it
# without losing that. So this retrofits the same wiring:
#
#   - the CA goes onto the node with `docker cp` (no extraMounts to add)
#   - the API server's static-pod manifest is patched in place; the kubelet
#     notices and restarts it
#   - a host-side `kubectl port-forward` stands in for the extraPortMapping
#     that cannot be added after creation
#
# The port-forward is the one ongoing process: it binds 127.0.0.1:32000 on
# the host, which is the SAME url the API server uses inside the node via the
# NodePort. Same string for both, which is what keeps `iss` matching.
attach_oidc() {
  local node="${CLUSTER}-control-plane"
  local manifest=/etc/kubernetes/manifests/kube-apiserver.yaml

  kind get clusters 2>/dev/null | grep -qx "$CLUSTER" \
    || { warn "cluster $CLUSTER does not exist"; exit 1; }

  if docker exec "$node" grep -q 'oidc-issuer-url' "$manifest" 2>/dev/null; then
    log "$CLUSTER already has OIDC flags, leaving the manifest alone"
  else
    log "copying the CA onto $node"
    docker exec "$node" mkdir -p /etc/oidc
    docker cp "$WORK_DIR/ca.pem" "$node:/etc/oidc/ca.pem"

    log "backing up and patching the API server manifest"
    # The backup goes OUTSIDE /etc/kubernetes/manifests/. The kubelet parses
    # every file in that directory regardless of extension, so a
    # `kube-apiserver.yaml.pre-oidc` sitting next to the real manifest is a
    # SECOND declaration of the same static pod — and the backup won, so the
    # patched flags never took effect while every check said the API server
    # was healthy. Cost an hour of chasing a token that was already perfect.
    docker exec "$node" cp "$manifest" /etc/kubernetes/kube-apiserver.yaml.pre-oidc
    docker exec "$node" cat "$manifest" > "$WORK_DIR/apiserver.yaml"

    python3 - "$WORK_DIR/apiserver.yaml" "$ISSUER" "$CLIENT_ID" <<'PYEOF'
import sys, yaml
path, issuer, client_id = sys.argv[1], sys.argv[2], sys.argv[3]
d = yaml.safe_load(open(path))
c = d['spec']['containers'][0]

flags = [
    f'--oidc-issuer-url={issuer}',
    f'--oidc-client-id={client_id}',
    '--oidc-ca-file=/etc/oidc/ca.pem',
    '--oidc-username-claim=preferred_username',
    '--oidc-username-prefix=oidc:',
    '--oidc-groups-claim=groups',
    '--oidc-groups-prefix=oidc:',
]
# Idempotent: drop any existing oidc flag before adding ours back.
c['command'] = [a for a in c['command'] if not a.startswith('--oidc-')] + flags

# The static pod only sees host paths it is explicitly given — the same
# two-boundary problem `up` solves with kubeadm extraVolumes.
c.setdefault('volumeMounts', [])
if not any(m.get('name') == 'oidc-ca' for m in c['volumeMounts']):
    c['volumeMounts'].append({
        'name': 'oidc-ca', 'mountPath': '/etc/oidc/ca.pem', 'readOnly': True})
d['spec'].setdefault('volumes', [])
if not any(v.get('name') == 'oidc-ca' for v in d['spec']['volumes']):
    d['spec']['volumes'].append({
        'name': 'oidc-ca',
        'hostPath': {'path': '/etc/oidc/ca.pem', 'type': 'File'}})

yaml.safe_dump(d, open(path, 'w'), default_flow_style=False)
PYEOF

    docker cp "$WORK_DIR/apiserver.yaml" "$node:${manifest}"
    log "  patched; the kubelet will restart the API server"
  fi

  log "waiting for the API server to come back"
  # /healthz alone is worthless here: if the kubelet never applies the new
  # spec the OLD API server stays up and answers healthy forever, so the
  # check passes precisely when the patch failed. Assert the flags are in
  # the RUNNING process afterwards.
  local deadline=$((SECONDS + 180))
  until kubectl --context "kind-${CLUSTER}" get --raw /healthz >/dev/null 2>&1; do
    if (( SECONDS > deadline )); then
      warn "the API server did not come back within 180s."
      warn "Restore it by hand and the cluster is as it was:"
      warn "  docker exec $node cp /etc/kubernetes/kube-apiserver.yaml.pre-oidc $manifest"
      exit 1
    fi
    sleep 3
  done
  # The check that actually means something.
  local oidc_deadline=$((SECONDS + 180))
  until [[ "$(running_oidc_flag_count)" -ge 1 ]]; do
    if (( SECONDS > oidc_deadline )); then
      warn "the API server is healthy but is running WITHOUT the OIDC flags."
      warn "  The kubelet has not applied the patched manifest. Check that"
      warn "  /etc/kubernetes/manifests/ contains only real manifests — the"
      warn "  kubelet parses every file there, so a stray backup declaring"
      warn "  the same static pod will win over the one you edited."
      exit 1
    fi
    sleep 3
  done
  log "  ✓ API server running with OIDC in effect ($(running_oidc_flag_count) flags)"
}

# How many --oidc-* flags the RUNNING API server process actually has.
#
# Read from the container spec via crictl rather than /proc: a `grep
# kube-apiserver /proc/*/cmdline` also matches the shell running the grep,
# which makes any such loop believe the process is always present.
running_oidc_flag_count() {
  local node="${CLUSTER}-control-plane" cid
  cid="$(docker exec "$node" crictl ps --name kube-apiserver -q 2>/dev/null | head -1)"
  [[ -n "$cid" ]] || { echo 0; return; }
  docker exec "$node" crictl inspect "$cid" 2>/dev/null | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
    args = d.get("info", {}).get("runtimeSpec", {}).get("process", {}).get("args", [])
    print(len([a for a in args if a.startswith("--oidc")]))
except Exception:
    print(0)
' 2>/dev/null || echo 0
}

# Replace the GitHub OAuth App credentials on a cluster that is already set
# up, and restart Dex to pick them up.
#
# Exists because `attach` is worth running before you have an OAuth App — it
# proves the whole chain except the GitHub redirect — so the credentials
# arrive second.
set_github_creds() {
  require_github_app
  local kc=(kubectl --context "kind-${CLUSTER}")
  log "updating the GitHub credentials and restarting Dex"
  "${kc[@]}" -n dex create secret generic dex-github \
    --from-literal=client-id="$GITHUB_CLIENT_ID" \
    --from-literal=client-secret="$GITHUB_CLIENT_SECRET" \
    --dry-run=client -o yaml | "${kc[@]}" apply -f - >/dev/null
  "${kc[@]}" -n dex rollout restart deploy/dex >/dev/null
  "${kc[@]}" -n dex rollout status deploy/dex --timeout=120s
  # The socat proxy targets the NodePort so a pod replacement no longer
  # breaks it — but verify rather than assume, because this is exactly where
  # the old pod-bound port-forward died silently.
  if ! nc -z 127.0.0.1 "$NODE_PORT" 2>/dev/null; then
    warn "127.0.0.1:$NODE_PORT stopped answering; restarting the proxy"
    start_port_forward
  fi
  log "  ✓ Dex restarted. Now: $0 login"
}

# Stand in for the extraPortMapping a running cluster cannot be given.
#
# A socat container on kind's Docker network, publishing the port on the host
# and forwarding to the node's NodePort.
#
# NOT `kubectl port-forward`, which was the first attempt and is wrong here:
# it binds to a specific POD, so it dies the moment Dex restarts — and
# `github-creds` restarts Dex by design, which made the documented sequence
# (attach → github-creds → login) break itself. The failure surfaces as
# `connection refused` on the issuer URL, pointing at the cluster rather than
# at the forwarder that quietly exited.
#
# This targets the NodePort instead, which is the same path the API server
# uses from inside the node, and survives pod restarts, `rollout restart`,
# and (with --restart) a Docker restart.
start_port_forward() {
  local name="${CLUSTER}-dex-proxy"
  local node="${CLUSTER}-control-plane"
  local net
  net="$(docker inspect "$node" \
    --format '{{range $k,$v := .NetworkSettings.Networks}}{{$k}}{{end}}')"
  [[ -n "$net" ]] || { warn "could not determine $node's docker network"; exit 1; }

  docker rm -f "$name" >/dev/null 2>&1 || true
  log "publishing 127.0.0.1:$NODE_PORT -> ${node}:${NODE_PORT} via socat"
  docker run -d --name "$name" \
    --network "$net" \
    --restart unless-stopped \
    -p "127.0.0.1:${NODE_PORT}:${NODE_PORT}" \
    alpine/socat \
    "tcp-listen:${NODE_PORT},fork,reuseaddr" \
    "tcp-connect:${node}:${NODE_PORT}" >/dev/null

  local deadline=$((SECONDS + 30))
  until nc -z 127.0.0.1 "$NODE_PORT" 2>/dev/null; do
    if (( SECONDS > deadline )); then
      warn "the proxy never started listening. Logs:"
      docker logs "$name" 2>&1 | tail -5 >&2
      exit 1
    fi
    sleep 1
  done
  log "  ✓ 127.0.0.1:$NODE_PORT reaches Dex (container $name)"
}

# ---------------------------------------------------------------------------
# RBAC + policy fixtures for the logged-in human
# ---------------------------------------------------------------------------

grant_rbac() {
  local kc=(kubectl --context "kind-${CLUSTER}")
  log "granting claim access to authenticated OIDC users"
  # Bound to the group, not a username: the whole point is that you do not
  # know your OIDC username until you have logged in once.
  "${kc[@]}" apply -f - <<EOF >/dev/null
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
metadata: { name: banlieue-claim-author, namespace: $NAMESPACE }
roleRef: { apiGroup: rbac.authorization.k8s.io, kind: Role, name: banlieue-claim-author }
subjects:
  - kind: Group
    name: "system:authenticated"
    apiGroup: rbac.authorization.k8s.io
EOF
}

# The policy ships its parameter ConfigMap into this namespace, so it has to
# exist first. Separated from grant_rbac because the ordering is load-bearing
# and was wrong once: `install_banlieue` ran first and died on
# "namespaces banlieue-system not found".
ensure_namespace() {
  local kc=(kubectl --context "kind-${CLUSTER}")
  "${kc[@]}" create namespace "$NAMESPACE" --dry-run=client -o yaml \
    | "${kc[@]}" apply -f - >/dev/null
}

install_banlieue() {
  local kc=(kubectl --context "kind-${CLUSTER}")
  local root; root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  log "installing banlieue CRDs and the claim subject policy"
  "${kc[@]}" apply -f "$root/deploy/crds/" >/dev/null
  "${kc[@]}" apply -f "$root/deploy/admission/virtualmachineclaim-subject-authorization.yaml" >/dev/null
  log "  NOTE: the policy's 'issuers' allowlist ships as a placeholder."
  log "  '$0 login' rewrites it to this cluster's real issuer."
}

# ---------------------------------------------------------------------------
# Login
# ---------------------------------------------------------------------------

setup_login() {
  local kc=(kubectl --context "kind-${CLUSTER}")
  command -v kubectl-oidc_login >/dev/null 2>&1 || {
    warn "kubelogin is not installed. It runs the browser flow and caches the token:"
    warn "  kubectl krew install oidc-login"
    warn "  (or: brew install int128/kubelogin/kubelogin)"
    exit 1; }

  log "configuring a kubectl user that logs in through Dex"
  # One --exec-arg per scope, NEVER a comma-separated list. `set-credentials`
  # treats --exec-arg as a comma-split string slice, so
  # `--oidc-extra-scope=profile,email,groups` is stored as three args —
  # `--oidc-extra-scope=profile`, `email`, `groups` — and kubelogin reads the
  # bare `email` as a subcommand:
  #     error: unknown command "email" for "kubelogin get-token"
  # which points at kubelogin rather than at the kubeconfig that produced it.
  kubectl config set-credentials banlieue-oidc \
    --exec-api-version=client.authentication.k8s.io/v1beta1 \
    --exec-command=kubectl \
    --exec-arg=oidc-login --exec-arg=get-token \
    --exec-arg="--oidc-issuer-url=$ISSUER" \
    --exec-arg="--oidc-client-id=$CLIENT_ID" \
    --exec-arg="--oidc-client-secret=$CLIENT_SECRET" \
    --exec-arg="--oidc-extra-scope=profile" \
    --exec-arg="--oidc-extra-scope=email" \
    --exec-arg="--oidc-extra-scope=groups" \
    --exec-arg="--certificate-authority=$WORK_DIR/ca.pem" >/dev/null

  kubectl config set-context "banlieue-oidc" \
    --cluster="kind-${CLUSTER}" \
    --user=banlieue-oidc \
    --namespace="$NAMESPACE" >/dev/null

  log "opening a browser to authenticate with GitHub ..."
  if ! kubectl --context banlieue-oidc auth whoami; then
    # kubelogin decides whether a cached token is usable from its `exp`
    # alone. A token signed by a key that no longer exists is therefore
    # served happily and rejected by the API server -- and the flow is never
    # re-run, because as far as kubelogin is concerned the token is fine.
    # Any Dex key rotation puts it in this state, and no amount of re-running
    # `login` escapes it. Clearing the cache is the only way out.
    warn "the cached token was rejected -- clearing the kubelogin cache and retrying"
    rm -rf "${HOME}/.kube/cache/oidc-login"
    kubectl --context banlieue-oidc auth whoami
  fi

  local me raw
  me="$(kubectl --context banlieue-oidc auth whoami -o jsonpath='{.status.userInfo.username}')"
  raw="$(raw_subject)"
  log ""
  log "You are '$me' to this API server."
  log "A claim must set spec.subject.id to the RAW subject: '$raw'"
  log "  (without the API server's username prefix — the claim stores what a"
  log "   JWT carries, so the in-guest agent can compare it. ADR-0047 D9.)"

  # Point the policy's allowlist at this cluster's real issuer, so the
  # issuer check passes for tokens Dex actually mints.
  "${kc[@]}" -n "$NAMESPACE" patch configmap banlieue-claim-subject-policy \
    --type=merge -p "{\"data\":{\"issuers\":\"${ISSUER}\\n\"}}" >/dev/null
  log "policy issuer allowlist set to $ISSUER"
  log ""
  log "Try it:"
  log "  $0 try-claim"
}

# The authenticated username with this cluster's API-server prefix removed —
# i.e. the subject as the ISSUER spells it, which is what `spec.subject.id`
# stores and what a JWT actually carries (ADR-0047 Decision 9, ADR-0049).
raw_subject() {
  local prefix
  prefix="$(kubectl --context "kind-${CLUSTER}" -n "$NAMESPACE" \
    get configmap banlieue-claim-subject-policy \
    -o jsonpath='{.data.usernamePrefix}' 2>/dev/null | tr -d '\n')"
  kubectl --context banlieue-oidc auth whoami \
    -o jsonpath='{.status.userInfo.username}' \
    | sed "s|^${prefix}||"
}

# Create two claims as the logged-in human: one honest, one impersonating.
# The point is to see the policy accept the first and refuse the second.
try_claim() {
  local me
  me="$(raw_subject)"

  echo
  log "1/2 a claim naming YOU ($me, the raw subject) — expect this to be accepted"
  cat <<EOF | kubectl --context banlieue-oidc apply --dry-run=server -f - || true
apiVersion: banlieue.io/v1alpha1
kind: VirtualMachineClaim
metadata: { name: mine, namespace: $NAMESPACE }
spec:
  poolRef: { name: sandbox-pool }
  subject: { issuer: "$ISSUER", id: "$me" }
  ttlSeconds: 900
EOF

  echo
  log "2/2 the same claim naming SOMEBODY ELSE — expect Forbidden"
  cat <<EOF | kubectl --context banlieue-oidc apply --dry-run=server -f - || true
apiVersion: banlieue.io/v1alpha1
kind: VirtualMachineClaim
metadata: { name: theirs, namespace: $NAMESPACE }
spec:
  poolRef: { name: sandbox-pool }
  subject: { issuer: "$ISSUER", id: "somebody-else" }
  ttlSeconds: 900
EOF
  echo
}

status() {
  echo "cluster:  $(kind get clusters 2>/dev/null | grep -x "$CLUSTER" || echo '(absent)')"
  echo "issuer:   $ISSUER"
  echo "work dir: $WORK_DIR"
  echo -n "proxy:    "; docker ps --filter "name=${CLUSTER}-dex-proxy" \
    --format '{{.Names}} {{.Status}}' | head -1 || true
  echo -n "listening:"; (nc -z 127.0.0.1 "$NODE_PORT" 2>/dev/null \
    && echo " 127.0.0.1:$NODE_PORT open") || echo " DOWN"
  kubectl --context "kind-${CLUSTER}" -n dex get pods 2>/dev/null || true
  kubectl --context banlieue-oidc auth whoami 2>/dev/null \
    || echo "not logged in (run: $0 login)"
}

teardown() {
  log "removing the dex proxy container"
  docker rm -f "${CLUSTER}-dex-proxy" >/dev/null 2>&1 || true
  log "deleting cluster $CLUSTER"
  kind delete cluster --name "$CLUSTER" || true
  kubectl config delete-context banlieue-oidc >/dev/null 2>&1 || true
  kubectl config delete-user banlieue-oidc >/dev/null 2>&1 || true
  # The CA and key are dev-only but are still key material; do not leave them.
  rm -rf "$WORK_DIR"
  log "removed $WORK_DIR"
  if kind get clusters 2>/dev/null | grep -qx "$CLUSTER"; then
    warn "cluster $CLUSTER is STILL present — delete it by hand"
    exit 1
  fi
  log "clean"
}

main() {
  case "${1:-up}" in
    up)
      check_deps; require_github_app
      make_certs; create_cluster; deploy_dex; verify_discovery
      ensure_namespace; install_banlieue; grant_rbac
      log ""
      log "Cluster is ready. Now: $0 login"
      ;;
    attach)
      # Retrofit onto an existing cluster, keeping whatever it already runs.
      check_deps; require_github_app
      make_certs; deploy_dex; attach_oidc; verify_discovery
      ensure_namespace; install_banlieue; grant_rbac; start_port_forward
      log ""
      log "Attached. Now: $0 login"
      ;;
    github-creds) check_deps; set_github_creds ;;
    certs)     check_deps; make_certs ;;
    dex)       check_deps; deploy_dex; verify_discovery ;;
    verify)    check_deps; verify_discovery ;;
    login)     check_deps; setup_login ;;
    try-claim) check_deps; try_claim ;;
    status)    status ;;
    down)      teardown ;;
    *)
      echo "Usage: $0 [up|attach|github-creds|login|try-claim|status|down|certs|dex|verify]" >&2
      echo "" >&2
      echo "  up         create a NEW cluster with OIDC baked in at creation" >&2
      echo "  attach     retrofit OIDC onto an EXISTING cluster (CLUSTER=<name>)," >&2
      echo "             keeping its Providers, pools and running VMs" >&2
      echo "  github-creds  swap in real OAuth App credentials and restart Dex" >&2
      echo "  login      browser flow through GitHub; prints your OIDC username" >&2
      echo "  try-claim  create one honest and one impersonating claim" >&2
      echo "  status     what exists and who you are" >&2
      echo "  down       delete the cluster and the dev CA" >&2
      echo "" >&2
      echo "Needs GITHUB_CLIENT_ID / GITHUB_CLIENT_SECRET from a GitHub OAuth App" >&2
      echo "whose callback URL is ${ISSUER}/callback" >&2
      exit 1
      ;;
  esac
}

main "$@"
