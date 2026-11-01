#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Tests for scripts/prelay-kairos-raw.sh that need no root, KVM or image: the
# GRUB environment block writer, and the argument checks that run before the
# script touches anything. The build boot itself is exercised live (ADR-0113;
# roadmap 18 phase C), because only a real Kairos boot can say whether the
# reset ran.
#
# Usage: scripts/prelay-kairos-raw-test.sh
set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/prelay-kairos-raw.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

failures=0
ok() { echo "ok   $1"; }
fail() { echo "FAIL $1"; failures=$((failures + 1)); }
check() { # case name, then a command that must succeed
  local name=$1; shift
  if "$@"; then ok "$name"; else fail "$name"; fi
}

# shellcheck source=prelay-kairos-raw.sh
PRELAY_SOURCE_ONLY=1 source "$SCRIPT"
set +e

# ---- grubenv_set ------------------------------------------------------------

env="$WORK/grubenv"
grubenv_set "$env" timeout 0
check "a new block is exactly 1024 bytes" test "$(wc -c <"$env")" -eq 1024
check "a new block starts with GRUB's header" test "$(head -n1 "$env")" = "# GRUB Environment Block"
check "a new block carries the variable" grep -qx 'timeout=0' "$env"

# The block AuroraBoot writes to COS_OEM: next_entry, empty.
printf '# GRUB Environment Block\nnext_entry=\n' >"$env"
printf '%*s' $((1024 - $(wc -c <"$env"))) '' | tr ' ' '#' >>"$env"
grubenv_set "$env" timeout 0
check "existing variables are kept" grep -qx 'next_entry=' "$env"
check "the timeout is added" grep -qx 'timeout=0' "$env"
check "the result is still 1024 bytes" test "$(wc -c <"$env")" -eq 1024

grubenv_set "$env" timeout 0
check "setting it again does not duplicate it" test "$(grep -c '^timeout=' "$env")" -eq 1
grubenv_set "$env" timeout 5
check "setting it again replaces the value" grep -qx 'timeout=5' "$env"
check "and leaves no old value" test "$(grep -c '^timeout=' "$env")" -eq 1

if command -v grub-editenv >/dev/null; then
  grubenv_set "$env" timeout 0
  check "grub-editenv reads the block" test "$(grub-editenv "$env" list | sort | tr '\n' ' ')" = "next_entry= timeout=0 "
else
  echo "skip grub-editenv cross-check (not installed)"
fi

# ---- verify_checksum ------------------------------------------------------------

printf 'kairos' >"$WORK/blob"
good="sha256:$(sha256sum "$WORK/blob" | cut -d' ' -f1)"
check "a matching sha256 passes" bash -c "PRELAY_SOURCE_ONLY=1 source '$SCRIPT'; verify_checksum '$WORK/blob' '$good'"
check "a matching sha512 passes" bash -c "PRELAY_SOURCE_ONLY=1 source '$SCRIPT'; verify_checksum '$WORK/blob' 'sha512:$(sha512sum "$WORK/blob" | cut -d' ' -f1)'"
check "a mismatch fails" bash -c "! (PRELAY_SOURCE_ONLY=1 source '$SCRIPT'; verify_checksum '$WORK/blob' 'sha256:$(printf '0%.0s' {1..64})') 2>/dev/null"
check "md5 is refused" bash -c "! (PRELAY_SOURCE_ONLY=1 source '$SCRIPT'; verify_checksum '$WORK/blob' 'md5:0123') 2>/dev/null"
check "upper-case hex is refused" bash -c "! (PRELAY_SOURCE_ONLY=1 source '$SCRIPT'; verify_checksum '$WORK/blob' '${good^^}') 2>/dev/null"

# ---- argument checks (before anything is touched) ---------------------------

expect_fail() { # case name, expected message fragment, args...
  local name=$1 want=$2; shift 2
  local out
  out=$(bash "$SCRIPT" "$@" 2>&1) && { fail "$name: succeeded"; return; }
  if [[ $out == *"$want"* ]]; then ok "$name"; else fail "$name: got '$out'"; fi
}

touch "$WORK/in.raw" "$WORK/exists.raw"
expect_fail "no arguments is a usage error" "usage:"
expect_fail "too many arguments is a usage error" "usage:" a b 20 extra
expect_fail "a non-numeric size is refused" "positive number" "$WORK/in.raw" "$WORK/out.raw" big
expect_fail "a zero size is refused" "positive number" "$WORK/in.raw" "$WORK/out.raw" 0
expect_fail "a missing input is refused" "not found" "$WORK/nope.raw" "$WORK/out.raw"
expect_fail "an existing output is never overwritten" "exists" "$WORK/in.raw" "$WORK/exists.raw"

# The Job passes its arguments through the environment.
out=$(PRELAY_INPUT="$WORK/nope.raw" PRELAY_OUTPUT="$WORK/out.raw" bash "$SCRIPT" 2>&1)
if [[ $out == *"not found"* ]]; then ok "arguments can come from PRELAY_INPUT/PRELAY_OUTPUT"; else fail "env arguments: got '$out'"; fi
out=$(PRELAY_INPUT="$WORK/in.raw" bash "$SCRIPT" 2>&1)
if [[ $out == *"usage:"* ]]; then ok "PRELAY_INPUT without PRELAY_OUTPUT is a usage error"; else fail "env without output: got '$out'"; fi

if ((failures > 0)); then
  echo "$failures failure(s)"
  exit 1
fi
echo "all passed"
