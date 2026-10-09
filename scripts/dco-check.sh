#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Developer Certificate of Origin check (roadmap 12 §4.9, CONTRIBUTING.md).
#
# Every non-merge commit in RANGE must carry a `Signed-off-by:` trailer whose
# name and email match the commit's author. A sign-off for someone else does
# not certify the author's own contribution, so it does not count. Matching
# ignores case, as email addresses are compared by the DCO GitHub App.
#
# One exception: a GitHub App bot (author name ending in "[bot]") commits as
# its noreply address but signs off with another (Dependabot signs off as
# support@github.com), so for a bot a sign-off with the same name counts.
#
# Usage: scripts/dco-check.sh <git revision range>
#   e.g. scripts/dco-check.sh origin/main..HEAD
#
# Exit status: 0 when every commit is signed off, 1 when any is not, 2 on a
# usage error.
set -euo pipefail

range="${1:-}"
if [ -z "$range" ]; then
  echo "usage: $0 <git revision range>" >&2
  exit 2
fi

missing=0
checked=0
while read -r sha; do
  [ -n "$sha" ] || continue
  checked=$((checked + 1))
  name="$(git log -1 --format='%an' "$sha")"
  author="$name <$(git log -1 --format='%ae' "$sha")>"
  signoffs="$(git log -1 --format='%(trailers:key=Signed-off-by,valueonly)' "$sha")"
  if printf '%s\n' "$signoffs" | grep -Fxiq -- "$author"; then
    continue
  fi
  case "$name" in
    *'[bot]')
      if printf '%s\n' "$signoffs" | grep -Fiq -- "$name <"; then
        continue
      fi
      ;;
  esac
  missing=$((missing + 1))
  echo "::error::$(git log -1 --format='%h %s' "$sha") has no 'Signed-off-by: $author'"
done < <(git rev-list --no-merges "$range")

if [ "$missing" -gt 0 ]; then
  echo "DCO: $missing of $checked commit(s) not signed off by their author." >&2
  echo "Fix with: git rebase --signoff -S <base> && git push --force-with-lease" >&2
  exit 1
fi
echo "DCO: all $checked commit(s) signed off by their author."
