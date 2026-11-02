#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# check-infra-identifiers.sh: fail if a tracked file names real
# infrastructure (rules/no-real-infrastructure.md).
#
# Two layers:
#
#   1. Generic patterns, committed and run in CI. They name nothing real, so
#      the check itself publishes nothing:
#        - a host under the maintainer's mail domain (`<host>.jeb.ca`; the
#          domain alone is the documented author address);
#        - a Tailscale MagicDNS name (`<host>.<tailnet>.ts.net`), except the
#          `tailnet.ts.net` documentation placeholder;
#        - a single tailnet address (100.64.0.0/10). Network blocks written
#          with a prefix length (`100.64.0.0/10`) are design, not hosts.
#          (Each line gets a trailing space so "a non-digit after the prefix"
#          works the same in BSD and GNU grep: BSD's `$` is no anchor inside
#          an alternation.)
#
#   2. A local denylist the repository never sees: one extended regular
#      expression per line in $INFRA_DENYLIST (default
#      ~/.config/banlieue/infra-denylist), matched case-insensitively as whole
#      words. That is where real short host names go: committing them here
#      would publish what this check exists to keep out. Lines starting with
#      `#` are comments. Skipped when the file does not exist (CI).
#
# Usage: scripts/check-infra-identifiers.sh
set -euo pipefail

readonly DEFAULT_DENYLIST="${HOME:-}/.config/banlieue/infra-denylist"
readonly TAILNET_OCTETS='100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.[0-9]{1,3}\.[0-9]{1,3}'
# For git grep (its own regex engine): not preceded by a digit or a dot.
readonly TAILNET_IP="(^|[^0-9.])${TAILNET_OCTETS}"
# Files that use a denylisted word for something else (a colour palette).
readonly DENYLIST_EXCLUDES=(
  ':!docs/src/stylesheets/extra.css'
  ':!docs/mkdocs.yml'
  ':!.github/community/08-phase-1e-docs.md'
)

found=0
report() { # label, matches
  [[ -z $2 ]] && return 0
  echo "check-infra-identifiers: $1:" >&2
  printf '%s\n' "$2" | sed 's/^/  /' >&2
  found=1
}

report "a host under the maintainer's domain" \
  "$(git grep -n -I -i -E '[a-z0-9-]+\.jeb\.ca' || true)"
report "a Tailscale MagicDNS name" \
  "$(git grep -n -I -i -E '[a-z0-9-]+\.ts\.net' | grep -v -i -E '(^|[^a-z0-9-])tailnet\.ts\.net' || true)"
# Single addresses only: a block written with a prefix length is design.
tailnet_hosts() {
  git grep -n -I -E "$TAILNET_IP" | sed 's/$/ /' |
    grep -v -E "${TAILNET_OCTETS}/[0-9]{1,2}[^0-9]" | sed 's/ $//'
}
report "a tailnet address (100.64.0.0/10)" "$(tailnet_hosts || true)"

denylist=${INFRA_DENYLIST:-$DEFAULT_DENYLIST}
if [[ -f $denylist ]]; then
  patterns=$(grep -v -E '^[[:space:]]*(#|$)' "$denylist" || true)
  if [[ -n $patterns ]]; then
    report "a name from $denylist" \
      "$(git grep -n -I -i -w -E -f <(printf '%s\n' "$patterns") -- . "${DENYLIST_EXCLUDES[@]}" || true)"
  fi
fi

if ((found)); then
  echo "check-infra-identifiers: use the placeholders in .claude/rules/no-real-infrastructure.md" >&2
  exit 1
fi
echo "check-infra-identifiers: clean"
