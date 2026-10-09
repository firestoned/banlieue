#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Tests for scripts/dco-check.sh. Each case builds commits in a throwaway
# repository and asserts the check's exit status, so a change to the matching
# rules cannot quietly start passing unsigned work or blocking Dependabot.
#
# Usage: scripts/dco-check-test.sh
set -euo pipefail

CHECK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/dco-check.sh"
REPO="$(mktemp -d)"
trap 'rm -rf "$REPO"' EXIT
cd "$REPO"

# Isolate from the caller's git config (signing, hooks, default branch).
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
git init -q
git -c user.name=Base -c user.email=base@example.com commit -q --allow-empty -m base
git branch base

failures=0

commit() { # name, email, trailer (may be empty)
  git -c user.name="$1" -c user.email="$2" commit -q --allow-empty -m "change" ${3:+-m "$3"}
}

expect() { # case name, expected exit status
  local status=0
  bash "$CHECK" base..HEAD >/dev/null 2>&1 || status=$?
  if [ "$status" -eq "$2" ]; then
    echo "ok   $1"
  else
    echo "FAIL $1: exit $status, expected $2"
    failures=$((failures + 1))
  fi
  git reset -q --hard base
}

commit "Ada Lovelace" ada@example.com "Signed-off-by: Ada Lovelace <ada@example.com>"
expect "signed off by the author" 0

commit "Ada Lovelace" Ada@Example.COM "Signed-off-by: Ada Lovelace <ada@example.com>"
expect "email compared without case" 0

commit "Ada Lovelace" ada@example.com ""
expect "no sign-off" 1

commit "Ada Lovelace" ada@example.com "Signed-off-by: Bob <bob@example.com>"
expect "signed off by someone else" 1

commit "dependabot[bot]" "49699333+dependabot[bot]@users.noreply.github.com" \
  "Signed-off-by: dependabot[bot] <support@github.com>"
expect "Dependabot: same bot name, different address" 0

commit "evil[bot]" bot@example.com "Signed-off-by: Mallory <mallory@example.com>"
expect "bot signed off under another name" 1

commit "Ada Lovelace" ada@example.com "Signed-off-by: Ada Lovelace <ada@example.com>"
commit "Bob" bob@example.com ""
expect "one bad commit among good ones" 1

expect "empty range" 0

status=0
bash "$CHECK" >/dev/null 2>&1 || status=$?
if [ "$status" -eq 2 ]; then echo "ok   missing range is a usage error"; else
  echo "FAIL missing range: exit $status, expected 2"; failures=$((failures + 1)); fi

if [ "$failures" -gt 0 ]; then
  echo "$failures DCO check test(s) failed" >&2
  exit 1
fi
echo "All DCO check tests passed."
