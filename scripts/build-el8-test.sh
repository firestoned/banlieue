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

echo
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
