# Local Development

Build banlieue from source and run it against a local cluster — no released
image, no real vCenter. For installing a **release** on a real cluster, use the
[Guides](../guides/index.md) instead.

## Prerequisites

- Rust toolchain (the workspace MSRV is **1.88+**; `rustup` will honour
  `rust-version`).
- `kubectl`, and [`kind`](https://kind.sigs.k8s.io/) for a throwaway cluster.
- `docker` (for `vcsim` and for building the container image).

```sh
git clone https://github.com/firestoned/banlieue
cd banlieue
```

## The single binary

There is exactly one executable, `banlieue`; the controller and each provider are
subcommands ([ADR-0004](https://github.com/firestoned/banlieue/blob/main/docs/adr/0004-single-binary-subcommand-dispatch.md)).
The role crates (`banlieue-controller`, `banlieue-provider-*`) are libraries.

```sh
cargo run -p banlieue -- controller            # run the controller
cargo run -p banlieue -- provider vsphere      # run the vSphere provider
cargo run -p banlieue -- completion zsh        # emit a shell-completion script
cargo run -p banlieue -- --help
```

## Generate and apply the CRDs

CRDs are code-first — generated from the Rust types in `crates/banlieue-api`:

```sh
make crds                       # writes deploy/crds/ and the API reference
kubectl apply -f deploy/crds/
```

Inspect the generated API:

```sh
kubectl explain virtualmachine.spec
kubectl explain provider.spec.connection
```

## Run against a kind cluster

```sh
make kind-up                    # create a kind cluster + apply CRDs

# Run the controller out-of-cluster against your current kube-context:
make run-local                  # = cargo run -p banlieue -- controller
RUST_LOG=debug,kube=debug make run-local     # RUST_LOG is overridable
```

`make kind-up` applies the CRDs but runs the controller locally so you get fast
edit-compile-run cycles. To instead build the image and load it into kind:

```sh
make kind-load                  # cross-compile + build the single banlieue image
make kind-deploy-controller     # apply manifests, override image to the local build
make kind-deploy-provider-vsphere
```

## vSphere provider against `vcsim`

You don't need a real vCenter. The [`vcsim`](https://github.com/vmware/govmomi/tree/main/vcsim)
simulator ships a default inventory and speaks the same VI JSON API.

```sh
make kind-up
make vcsim-up                   # vmware/vcsim on :8989 (user: user / pass: pass)

kubectl create secret generic vcsim-creds \
  --from-literal=username=user \
  --from-literal=password=pass

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

make provider-vsphere-run-local   # cargo run -p banlieue --features vcsim -- provider vsphere --no-leader-elect
```

The `provider-vsphere-run-local` target builds with the `vcsim` feature, which
tolerates the simulator's known divergences from production vCenter. Override
logging with `RUST_LOG=debug,kube=debug make provider-vsphere-run-local`. Tear
down with `make vcsim-down`.

After a few seconds the `Provider` should report failure domains:

```sh
kubectl get provider dev-vcsim -o yaml | yq '.status.failureDomains'
```

### From your GOVC environment

If you already talk to vCenter (or `vcsim`) with
[`govc`](https://github.com/vmware/govmomi/tree/main/govc), the connection
details are in your shell as `GOVC_*` env vars. The provider does **not** read
these itself (connection is declared on the `Provider` spec — explicit over
implicit), but you can generate the Secret and `Provider` from them:

| `GOVC_*` env var | Maps to |
| --- | --- |
| `GOVC_URL` | `Provider.spec.connection.endpoint` |
| `GOVC_USERNAME` | Secret key `username` |
| `GOVC_PASSWORD` | Secret key `password` |
| `GOVC_INSECURE` (`1`/`true`) | `Provider.spec.connection.insecureSkipTLSVerify` |

```sh
kubectl create secret generic vsphere-creds \
  --from-literal=username="$GOVC_USERNAME" \
  --from-literal=password="$GOVC_PASSWORD"

# Normalise GOVC_URL into a full SDK URL (bare host / no scheme / user:pass@ / trailing /sdk):
host="${GOVC_URL#*://}"; host="${host#*@}"; host="${host%/sdk}"
endpoint="https://${host}/sdk"
case "${GOVC_INSECURE:-}" in 1|true|TRUE|yes) insecure=true ;; *) insecure=false ;; esac

cat <<EOF | kubectl apply -f -
apiVersion: banlieue.io/v1alpha1
kind: Provider
metadata: { name: dev-vsphere }
spec:
  providerClassRef: { name: vsphere }
  connection:
    endpoint: ${endpoint}
    credentialsRef: { name: vsphere-creds }
    insecureSkipTLSVerify: ${insecure}
EOF
```

## Quality gate

After any Rust change (non-negotiable):

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Build the docs

```sh
make docs            # regenerates the CRD API reference + CALM diagrams, builds the site
```

## Shell completion

The `banlieue` binary emits completion scripts for `bash`, `zsh`, `fish`,
`elvish`, and `powershell`:

=== "zsh"

    ```sh
    banlieue completion zsh > "${fpath[1]}/_banlieue"   # then restart your shell
    ```

=== "bash"

    ```sh
    banlieue completion bash > /etc/bash_completion.d/banlieue
    # or, for the current shell:
    source <(banlieue completion bash)
    ```

=== "fish"

    ```sh
    banlieue completion fish > ~/.config/fish/completions/banlieue.fish
    ```
