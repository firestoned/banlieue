#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Build the `banlieue` binary against glibc 2.28, so it runs on RHEL 8.
#
# The release binary is built on a current distribution and needs a newer glibc
# than RHEL 8 ships (2.28), and a RHEL 8 host usually cannot build it itself
# (its rustc package trails the workspace MSRV, and it has no internet). So the
# build runs off-host, in an el8 container:
#
#   1. `cargo vendor` on the machine running this script (needs network), so
#      the container builds --offline.
#   2. An el8 toolchain image (EL8_IMAGE + gcc + rustup at the workspace MSRV),
#      built once and cached under a tag that names the toolchain.
#   3. `cargo build --release -p banlieue` in that image, with the same
#      build-path remapping the release build uses.
#   4. A check that no symbol needs a glibc newer than EL8_MAX_GLIBC.
#
# The result is target/el8/banlieue. Copy it to the host and give it to
# `banlieue host cloud-hypervisor install --provider-binary`.
#
# Usage: scripts/build-el8.sh            (or: make build-el8)
#
# Settings (environment; none is required):
#   EL8_IMAGE      base image            (default almalinux:8; name a mirror here)
#   EL8_PLATFORM   container platform    (default linux/amd64; arm64 hosts emulate it)
#   EL8_PROXY      http(s) proxy for the toolchain image and the build, as the
#                  CONTAINER sees it (a proxy on the workstation's loopback is
#                  not reachable: use host.docker.internal or similar)
#   EL8_CA_FILE    PEM bundle to trust inside the container (a TLS-intercepting
#                  proxy needs one)
#   EL8_JOBS       cargo parallelism     (default 2)
#   EL8_OUT        output directory      (default target/el8)
#   EL8_DOCKER     container runtime     (default docker)
#   EL8_DRY_RUN=1  print the plan and run nothing
#
# The container runtime needs at least 12 GiB of memory (16 is comfortable):
# the `vim_rs` crate is OOM-killed in 8 GiB. Colima: `colima start --memory 16`.
set -euo pipefail

# glibc symbol versions are compared as versions, not text (2.9 < 2.28).
readonly EL8_MAX_GLIBC="2.28"
readonly BYTES_PER_GIB=1073741824
readonly EL8_MIN_MEMORY_GIB=12
readonly EL8_DEFAULT_JOBS=2
readonly DNF_ATTEMPTS=12
readonly DNF_RETRIES=10
readonly DNF_TIMEOUT_SECS=60
readonly DNF_BACKOFF_SECS=15
readonly CONTAINER_PATH="/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

# glibc_within <found> <ceiling>: true when <found> <= <ceiling> as versions.
glibc_within() {
  local found="$1" ceiling="$2" highest
  highest="$(printf '%s\n%s\n' "$found" "$ceiling" | sort -V | tail -1)"
  [ "$highest" = "$ceiling" ]
}

# memory_enough <bytes>: true when the container runtime has enough memory.
memory_enough() {
  [ "$1" -ge $((EL8_MIN_MEMORY_GIB * BYTES_PER_GIB)) ]
}

die() {
  echo "build-el8: $*" >&2
  exit 1
}

# The workspace MSRV is the toolchain: the el8 host's own rustc trails it.
workspace_rust_version() {
  local root="$1" version
  version="$(sed -n 's/^rust-version *= *"\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
  [ -n "$version" ] || die "no rust-version in $root/Cargo.toml"
  echo "$version"
}

# run <command...>: print it under EL8_DRY_RUN, else run it.
run() {
  if [ -n "${EL8_DRY_RUN:-}" ]; then
    echo "+ $*"
    return 0
  fi
  "$@"
}

main() {
  local root image platform out jobs docker proxy ca rust tag
  root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  image="${EL8_IMAGE:-almalinux:8}"
  platform="${EL8_PLATFORM:-linux/amd64}"
  out="${EL8_OUT:-$root/target/el8}"
  jobs="${EL8_JOBS:-$EL8_DEFAULT_JOBS}"
  docker="${EL8_DOCKER:-docker}"
  proxy="${EL8_PROXY:-}"
  ca="${EL8_CA_FILE:-}"

  case "$jobs" in '' | *[!0-9]*) die "EL8_JOBS must be a number, got '$jobs'" ;; esac
  if [ -n "$ca" ] && [ ! -r "$ca" ]; then
    die "EL8_CA_FILE is not readable: $ca"
  fi
  if [ -z "${EL8_DRY_RUN:-}" ] && ! command -v "$docker" >/dev/null 2>&1; then
    die "container runtime not found: EL8_DOCKER=$docker"
  fi

  rust="$(workspace_rust_version "$root")"
  tag="banlieue-el8-toolchain:$rust"

  local -a proxy_build=() proxy_run=()
  if [ -n "$proxy" ]; then
    proxy_build=(--build-arg "http_proxy=$proxy" --build-arg "https_proxy=$proxy")
    proxy_run=(-e "http_proxy=$proxy" -e "https_proxy=$proxy")
  fi

  if [ -z "${EL8_DRY_RUN:-}" ]; then
    local mem
    mem="$("$docker" info --format '{{.MemTotal}}' 2>/dev/null)" || die "cannot reach the container runtime ($docker info failed)"
    memory_enough "$mem" || die "the container runtime has $((mem / BYTES_PER_GIB)) GiB; at least $EL8_MIN_MEMORY_GIB are needed (the vim_rs crate is OOM-killed below that). Colima: colima start --memory 16"
  fi

  mkdir -p "$out/ctx"
  if [ -n "$ca" ]; then cp "$ca" "$out/ctx/ca.pem"; else : >"$out/ctx/ca.pem"; fi

  # 1. vendor on this machine, so the container never needs a registry.
  echo "==> vendoring crates into $out/vendor"
  run bash -c "cd '$root' && cargo vendor --locked '$out/vendor' >/dev/null"
  cat >"$out/vendor.toml" <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "/out/vendor"
EOF

  # 2. the el8 toolchain image, cached under a tag that names the toolchain.
  # Made with `run` + `commit` rather than `build`: a proxy reachable from a
  # container is not always reachable from the image builder's network.
  echo "==> toolchain image $tag (RUST_VERSION=$rust)"
  if [ -n "${EL8_DRY_RUN:-}" ] || ! "$docker" image inspect "$tag" >/dev/null 2>&1; then
    local setup_name="banlieue-el8-toolchain-setup"
    local setup
    setup="$(cat <<EOF
set -eu
if [ -s /tmp/ca.pem ]; then cp /tmp/ca.pem /etc/pki/ca-trust/source/anchors/extra-ca.pem && update-ca-trust; fi
ok=
for i in \$(seq 1 $DNF_ATTEMPTS); do
  if dnf -y -q --setopt=retries=$DNF_RETRIES --setopt=timeout=$DNF_TIMEOUT_SECS --setopt=max_parallel_downloads=1 install gcc gcc-c++ make cmake perl-interpreter binutils; then ok=1; break; fi
  sleep $DNF_BACKOFF_SECS
done
[ -n "\$ok" ] || { echo "dnf failed after $DNF_ATTEMPTS attempts" >&2; exit 1; }
command -v gcc >/dev/null && command -v objdump >/dev/null
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain "\$RUST_VERSION"
EOF
)"
    if [ -n "${EL8_DRY_RUN:-}" ]; then
      echo "+ $docker run --name $setup_name --platform $platform -v $out/ctx/ca.pem:/tmp/ca.pem:ro -e RUST_VERSION=$rust ${proxy_run[*]:-} $image sh -c <toolchain setup>"
      echo "+ $docker commit $setup_name $tag"
    else
      "$docker" rm -f "$setup_name" >/dev/null 2>&1 || true
      if ! "$docker" run --name "$setup_name" --platform "$platform" \
        -v "$out/ctx/ca.pem:/tmp/ca.pem:ro" -e "RUST_VERSION=$rust" ${proxy_run[@]+"${proxy_run[@]}"} \
        "$image" sh -c "$setup"; then
        "$docker" rm -f "$setup_name" >/dev/null 2>&1 || true
        die "the toolchain image could not be made (see the output above)"
      fi
      "$docker" commit --change "ENV PATH=$CONTAINER_PATH" "$setup_name" "$tag" >/dev/null
      "$docker" rm -f "$setup_name" >/dev/null
    fi
  fi

  # 3. build, offline, with the release build's path remapping.
  echo "==> building banlieue against glibc <= $EL8_MAX_GLIBC"
  local remap="--remap-path-prefix=/out/vendor=/cargo/registry --remap-path-prefix=/root/.cargo=/cargo --remap-path-prefix=/root/.rustup=/rustup"
  run "$docker" run --rm --platform "$platform" \
    -v "$root:/src:ro" -v "$out:/out" ${proxy_run[@]+"${proxy_run[@]}"} \
    -e "RUSTFLAGS=$remap" "$tag" \
    sh -c "cd /src && cargo --config /out/vendor.toml build --release --offline --locked -j $jobs -p banlieue --target-dir /out/target && cp /out/target/release/banlieue /out/banlieue"

  # 4. nothing in the binary may need a newer glibc than the ceiling.
  [ -n "${EL8_DRY_RUN:-}" ] && return 0
  local found
  found="$("$docker" run --rm --platform "$platform" -v "$out:/out:ro" "$tag" \
    sh -c "objdump -T /out/banlieue | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -Vu | tail -1")"
  glibc_within "$found" "$EL8_MAX_GLIBC" || die "$out/banlieue needs glibc $found, above the $EL8_MAX_GLIBC ceiling"
  echo "==> $out/banlieue (needs glibc $found at most)"
  "$docker" run --rm --platform "$platform" -v "$out:/out:ro" "$tag" /out/banlieue --version
}

# Sourced by the tests for the helpers; run only when executed.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main "$@"
fi
