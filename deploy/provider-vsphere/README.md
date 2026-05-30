# banlieue-provider-vsphere — local-dev flow

Phase 1B iteration 1: capability introspection only. The provider connects
to a vCenter (or `vcsim`), walks datacenters → clusters, and patches
`Provider.status.failureDomains[]` plus the `Ready` / `ProviderReachable`
conditions. VM lifecycle reconciliation lands in iteration 2.

## Quickstart against `vcsim`

```sh
# 1. Start the kind cluster + apply CRDs.
make kind-up

# 2. Start vcsim (govmomi simulator) on :8989.
make vcsim-up

# 3. Create a Secret with vcsim's default credentials.
kubectl create secret generic vcsim-creds \
  --from-literal=username=user \
  --from-literal=password=pass

# 4. Create a Provider pointing at vcsim. The provider binary picks this up
#    because spec.providerClassRef.name == "vsphere".
cat <<'EOF' | kubectl apply -f -
apiVersion: banlieue.io/v1alpha1
kind: Provider
metadata:
  name: dev-vcsim
spec:
  providerClassRef:
    name: vsphere
  connection:
    endpoint: https://127.0.0.1:8989/sdk
    credentialsRef:
      name: vcsim-creds
    insecureSkipTLSVerify: true
EOF

# 5. Run the provider locally (against your current kube-context).
#    --no-leader-elect skips the Lease so you can run alongside an in-cluster replica.
make provider-vsphere-run-local

# 6. After a few seconds, the Provider should have failureDomains in status.
kubectl get provider dev-vcsim -o yaml | yq '.status'
```

## Creating the Secret + Provider from your `GOVC_*` environment

If you already talk to vCenter (or `vcsim`) with
[`govc`](https://github.com/vmware/govmomi/tree/main/govc), the connection
details are in your shell as `GOVC_*` env vars. The provider does **not** read
these itself — connection details are declared on the `Provider` spec (explicit
over implicit) — but you can generate the Secret and Provider straight from them.

`GOVC_*` → banlieue mapping:

| `GOVC_*` env var | Maps to |
| --- | --- |
| `GOVC_URL` | `Provider.spec.connection.endpoint` |
| `GOVC_USERNAME` | Secret key `username` |
| `GOVC_PASSWORD` | Secret key `password` |
| `GOVC_INSECURE` (`1`/`true`) | `Provider.spec.connection.insecureSkipTLSVerify: true` |

```sh
# 1. Credentials Secret straight from the env. Piped to kubectl — never commit a
#    Secret manifest with real credentials.
kubectl create secret generic vsphere-creds \
  --from-literal=username="$GOVC_USERNAME" \
  --from-literal=password="$GOVC_PASSWORD"

# 2. Normalise GOVC_URL into a full SDK URL. GOVC_URL may be a bare host, omit
#    the scheme, embed user:pass@, or already end in /sdk — the provider needs
#    https://HOST[:PORT]/sdk.
host="${GOVC_URL#*://}"   # strip scheme if present
host="${host#*@}"         # strip embedded user:pass@ if present
host="${host%/sdk}"       # strip trailing /sdk if present
endpoint="https://${host}/sdk"

# 3. Map GOVC_INSECURE (1/true/yes) to insecureSkipTLSVerify.
case "${GOVC_INSECURE:-}" in 1|true|TRUE|yes) insecure=true ;; *) insecure=false ;; esac

# 4. Create the Provider.
cat <<EOF | kubectl apply -f -
apiVersion: banlieue.io/v1alpha1
kind: Provider
metadata:
  name: dev-vsphere
spec:
  providerClassRef:
    name: vsphere
  connection:
    endpoint: ${endpoint}
    credentialsRef:
      name: vsphere-creds
    insecureSkipTLSVerify: ${insecure}
EOF
```

> If `GOVC_USERNAME` / `GOVC_PASSWORD` are unset, your credentials may be encoded
> in `GOVC_URL` as `user:pass@host`, or you can run `govc env` to print the
> resolved values. For a CA-validated endpoint, drop `insecureSkipTLSVerify` and
> set `connection.caBundle` to your PEM-encoded CA instead.

## What you should see

```yaml
status:
  conditions:
    - lastTransitionTime: "2026-05-26T..."
      message: vCenter reachable; inventory walk succeeded
      observedGeneration: 1
      reason: Reconciled
      status: "True"
      type: ProviderReachable
    - lastTransitionTime: "2026-05-26T..."
      message: Provider reconciled
      observedGeneration: 1
      reason: Reconciled
      status: "True"
      type: Ready
  failureDomains:
    - name: dev-vcsim-dc-0-c0_h0       # vcsim default inventory
      labels: { dc: DC0, cluster: C0_H0 }
      attributes:
        raw: { datacenter: DC0, cluster: C0_H0 }
    # ... one entry per (DC, cluster) in the simulator
  observedGeneration: 1
```

If `Ready=False`, check the `reason`:

| Reason | What it means | Where to look |
| --- | --- | --- |
| `SecretMissing` | Provider.spec.connection.credentialsRef points at a Secret that doesn't exist | `kubectl get secret <name>` |
| `SecretInvalid` | Secret exists but is missing `username` / `password` keys, or they aren't UTF-8 | `kubectl get secret <name> -o yaml` |
| `ConnectFailed` | vim_rs couldn't log in — bad creds, bad endpoint, or TLS failure | provider logs (`make vcsim-logs` too) |
| `InventoryFailed` | Logged in OK but listing datacenters / clusters failed | provider logs |

## In-cluster deployment

```sh
make kind-load            # builds the single `banlieue` image (all roles)
make kind-deploy-provider-vsphere
kubectl -n banlieue-system logs deploy/banlieue-provider-vsphere -f
```

## Stopping

```sh
make vcsim-down
kubectl delete provider dev-vcsim
kubectl delete secret vcsim-creds
```

## Configuration

CLI flags (env-var fallbacks in `BANLIEUE_*`):

```
banlieue provider vsphere
  [--kubeconfig PATH]                 # $KUBECONFIG
  [--namespace NS]                    # scope to one namespace; cluster-wide when unset
  [--no-leader-elect]                 # default: leader-elect
  [--leader-election-namespace NS]    # default: banlieue-system
  [--leader-election-id ID]           # default: banlieue-provider-vsphere
  [--leader-election-identity ID]     # default: $POD_NAME / $HOSTNAME / "unknown"
  [--log-format json|text]            # default: text (configmap sets json in-cluster)
  [--log-level error|warn|info|debug|trace]
  [--health-port 8081]
  [--metrics-port 8080]
  [--vsphere-task-timeout-secs 600]   # placeholder; consumed in iter 2+
```

## Phase 1B iteration scope

Iteration 1 (this iteration) ships *only* the capability-introspection path —
the provider populates `failureDomains[]` so the main controller's scheduler
can place VMs against a Provider. The actual `VSphereMachine` VM-lifecycle
reconciliation (clone-from-template, customise, power on, status mirror)
lands in iteration 2.

Today's smoke-test boundary: the main controller still stops at
`Scheduled=False reason=ImageNotReady` until a `VMImage.status.perProvider[]`
entry is flipped to `ready=true` — that's part of iteration 2 as well.
