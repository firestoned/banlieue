#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Tests for scripts/check-infra-identifiers.sh. Each case commits one file to a
# throwaway repository and asserts the check's exit status, so a change to the
# patterns cannot quietly start passing a real identifier or failing a
# legitimate placeholder.
#
# Usage: scripts/check-infra-identifiers-test.sh
set -euo pipefail

CHECK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check-infra-identifiers.sh"
REPO="$(mktemp -d)"
trap 'rm -rf "$REPO"' EXIT
cd "$REPO"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
git init -q
DENY="$REPO/.denylist-outside-the-tree"
printf '# a comment\nhostalpha\n\n' >"$DENY"

failures=0
expect() { # case name, expected exit status, file content
  printf '%s\n' "$3" >case.txt
  git add case.txt
  local status=0
  INFRA_DENYLIST="$DENY" bash "$CHECK" >/dev/null 2>&1 || status=$?
  if [ "$status" -eq "$2" ]; then echo "ok   $1"; else
    echo "FAIL $1: exit $status, expected $2"
    failures=$((failures + 1))
  fi
  git rm -q --cached case.txt
}

# Fixtures are fake and assembled here, so no real-looking identifier sits
# in this file for the check (or a reader) to trip over.
DOMAIN="jeb.""ca"
TS="ts.""net"
TAILNET_ADDR="100.""99.0.1"
expect "a host under the domain fails" 1 "ssh bar.$DOMAIN"
expect "the author address passes" 0 "authors = [\"Erick Bourgeois <erick@$DOMAIN>\"]"
expect "a MagicDNS name fails" 1 "https://bar.example-tail.$TS"
expect "the tailnet placeholder passes" 0 "SAN_DNS=\"host.tailnet.$TS\""
expect "a tailnet address fails" 1 "endpoint: qemu+tls://$TAILNET_ADDR/system"
expect "a tailnet network block passes" 0 "one /19 from 100.64.0.0/10"
expect "a documentation address passes" 0 "endpoint: qemu+tls://192.0.2.10/system"
expect "a denylisted name fails" 1 "ssh hostalpha"
expect "a denylisted name in a longer word passes" 0 "hostalphabet"
expect "a denylisted name is case-insensitive" 1 "Hostalpha"
expect "a clean file passes" 0 "kubectl get provider ch-bar"

# With no denylist file the generic layer still runs (CI).
printf 'ssh hostalpha\n' >case.txt
git add case.txt
status=0
INFRA_DENYLIST="$REPO/nope" bash "$CHECK" >/dev/null 2>&1 || status=$?
if [ "$status" -eq 0 ]; then echo "ok   no denylist: only the generic patterns apply"; else
  echo "FAIL no denylist: exit $status"
  failures=$((failures + 1))
fi

if [ "$failures" -gt 0 ]; then
  echo "$failures failure(s)"
  exit 1
fi
echo "all passed"
