#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Tests for scripts/build-el8.sh. Pure: no container runtime is started. The
# plan is read from EL8_DRY_RUN, the helpers are sourced, and the refusals are
# checked against a runtime that does not exist.
#
# Usage: scripts/build-el8-test.sh
#
# The `bash -c '...' _ "$arg"` checks expand "$1" in the child on purpose:
# single quotes are the point.
# shellcheck disable=SC2016,SC1091
set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/build-el8.sh"
ROOT="$(cd "$(dirname "$SCRIPT")/.." && pwd)"

PASS=0
FAIL=0

check() { # check <name> <command...>: passes when the command succeeds
  local name="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    PASS=$((PASS + 1))
    echo "ok   $name"
    return
  fi
  FAIL=$((FAIL + 1))
  echo "FAIL $name"
}

check_fails_with() { # check_fails_with <name> <needle> <command...>
  local name="$1" needle="$2"
  shift 2
  local out
  if out="$("$@" 2>&1)"; then
    FAIL=$((FAIL + 1))
    echo "FAIL $name (expected a refusal)"
    return
  fi
  if grep -q -- "$needle" <<<"$out"; then
    PASS=$((PASS + 1))
    echo "ok   $name"
    return
  fi
  FAIL=$((FAIL + 1))
  echo "FAIL $name (refused, but the message lacks: $needle)"
  echo "     got: $out"
}


# shellcheck source=build-el8.sh
source "$SCRIPT"

# --- the plan ---------------------------------------------------------------
RUST_MSRV="$(sed -n 's/^rust-version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"

check "plan names the el8 base image and platform" \
  bash -c 'plan=$(EL8_DRY_RUN=1 "$1"); grep -q "almalinux:8" <<<"$plan" && grep -q "linux/amd64" <<<"$plan"' _ "$SCRIPT"
check "plan takes the toolchain from the workspace MSRV ($RUST_MSRV)" \
  bash -c 'EL8_DRY_RUN=1 "$1" | grep -q "RUST_VERSION=$2"' _ "$SCRIPT" "$RUST_MSRV"
check "EL8_IMAGE overrides the base image" \
  bash -c 'EL8_IMAGE=registry.example.com/almalinux:8 EL8_DRY_RUN=1 "$1" | grep -q "registry.example.com/almalinux:8"' _ "$SCRIPT"
check "no proxy is passed unless EL8_PROXY is set" \
  bash -c '! EL8_DRY_RUN=1 "$1" | grep -qi "proxy"' _ "$SCRIPT"
check "EL8_PROXY reaches the image build and the build run" \
  bash -c 'EL8_PROXY=http://proxy.example.com:3128 EL8_DRY_RUN=1 "$1" | grep -c "https_proxy=http://proxy.example.com:3128" | grep -q "^[2-9]"' _ "$SCRIPT"
check "the source tree is mounted read-only" \
  bash -c 'EL8_DRY_RUN=1 "$1" | grep -q ":/src:ro"' _ "$SCRIPT"
check "build paths are remapped like the release build" \
  bash -c 'EL8_DRY_RUN=1 "$1" | grep -q "remap-path-prefix"' _ "$SCRIPT"
check "the build is offline and locked" \
  bash -c 'p=$(EL8_DRY_RUN=1 "$1"); grep -q -- "--offline" <<<"$p" && grep -q -- "--locked" <<<"$p"' _ "$SCRIPT"

# --- refusals ---------------------------------------------------------------
check_fails_with "an unreadable EL8_CA_FILE is refused, naming the variable" "EL8_CA_FILE" \
  env EL8_CA_FILE=/nonexistent/ca.pem EL8_DRY_RUN=1 "$SCRIPT"
check_fails_with "a non-numeric EL8_JOBS is refused" "EL8_JOBS" \
  env EL8_JOBS=many EL8_DRY_RUN=1 "$SCRIPT"
check_fails_with "a missing container runtime is refused, naming it" "EL8_DOCKER" \
  env EL8_DOCKER=/nonexistent/docker "$SCRIPT"

# --- the glibc ceiling ------------------------------------------------------
check "glibc 2.27 is within a 2.28 ceiling" glibc_within 2.27 2.28
check "glibc 2.28 is within a 2.28 ceiling" glibc_within 2.28 2.28
if glibc_within 2.29 2.28; then
  FAIL=$((FAIL + 1))
  echo "FAIL glibc 2.29 must exceed a 2.28 ceiling"
else
  PASS=$((PASS + 1))
  echo "ok   glibc 2.29 exceeds a 2.28 ceiling"
fi
if glibc_within 2.39 2.28; then
  FAIL=$((FAIL + 1))
  echo "FAIL glibc 2.39 must exceed a 2.28 ceiling (the release binary's weak symbol is not a pass)"
else
  PASS=$((PASS + 1))
  echo "ok   glibc 2.39 exceeds a 2.28 ceiling"
fi
check "glibc 2.9 sorts below 2.28 (version order, not text order)" glibc_within 2.9 2.28

# --- the memory floor -------------------------------------------------------
check "16 GiB of container memory is enough" memory_enough 17179869184
if memory_enough 8589934592; then
  FAIL=$((FAIL + 1))
  echo "FAIL 8 GiB must be refused: vim_rs is OOM-killed there"
else
  PASS=$((PASS + 1))
  echo "ok   8 GiB is refused"
fi

# --- release wiring (ADR-0177) ---------------------------------------------
# The release builds the el8 binary in CI by the same recipe, and signs it like
# the other binaries. These read .github/workflows/build.yaml with yq.
WORKFLOW="$ROOT/.github/workflows/build.yaml"
EL8_ARTIFACT="banlieue-linux-amd64-el8"

wf() { yq "$1" "$WORKFLOW"; }

if ! command -v yq >/dev/null 2>&1; then
  FAIL=$((FAIL + 1))
  echo "FAIL yq is required to check the release wiring"
else
  check "the build-el8 job exists" \
    bash -c '[ "$(yq ".jobs | has(\"build-el8\")" "$1")" = true ]' _ "$WORKFLOW"
  check "the build-el8 job calls only the Makefile target" \
    bash -c 'yq ".jobs.build-el8.steps[].run | select(. != null)" "$1" | grep -qx "make build-el8" && ! yq ".jobs.build-el8.steps[].run | select(. != null)" "$1" | grep -v "^make " | grep -q .' _ "$WORKFLOW"
  check "the base image is pinned by digest" \
    bash -c 'yq ".jobs.build-el8.steps[].env.EL8_IMAGE | select(. != null)" "$1" | grep -Eq "@sha256:[0-9a-f]{64}$"' _ "$WORKFLOW"
  check "the job does not run on pull requests" \
    bash -c 'wf=$(yq ".jobs.build-el8.if" "$1"); grep -q "!= .pull_request." <<<"$wf"' _ "$WORKFLOW"
  check "a release stamps the tag version before building" \
    bash -c 'yq ".jobs.build-el8.steps[].run | select(. != null)" "$1" | grep -q "^make set-version"' _ "$WORKFLOW"
  check "the binary is uploaded as the $EL8_ARTIFACT artifact" \
    bash -c 'yq ".jobs.build-el8.steps[] | select(.with.name == \"$2\") | .with.path" "$1" | grep -q "target/el8/banlieue"' _ "$WORKFLOW" "$EL8_ARTIFACT"
  check "the el8 binary is signed with the other binaries" \
    bash -c 'yq ".jobs.sign-artifacts.strategy.matrix.artifact_name[]" "$1" | grep -qx "$2"' _ "$WORKFLOW" "$EL8_ARTIFACT"
  check "signing waits for the el8 build, so a failed build stops the release" \
    bash -c 'yq ".jobs.sign-artifacts.needs[]" "$1" | grep -qx "build-el8"' _ "$WORKFLOW"
  check "the SLSA subjects pattern covers the signed el8 tarball" \
    bash -c 'pattern=$(yq ".jobs.generate-provenance-subjects.steps[0].with.pattern" "$1"); case "$2-signed" in $pattern) true ;; *) false ;; esac' _ "$WORKFLOW" "$EL8_ARTIFACT"
  check "every action in the build-el8 job is pinned to a commit" \
    bash -c 'yq ".jobs.build-el8.steps[].uses | select(. != null)" "$1" | grep -Evq "@[0-9a-f]{40}( |$)" && exit 1; exit 0' _ "$WORKFLOW"
  check "the unit tests run build-el8-test in CI" \
    bash -c 'yq ".jobs.test.steps[].run | select(. != null)" "$1" | grep -qx "make build-el8-test"' _ "$WORKFLOW"
fi

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
