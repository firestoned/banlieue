#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# `make ch-host-install-test`: run `banlieue host cloud-hypervisor install` as root in a
# Debian 13 container, against real useradd, NSS, swtpm and the real pinned
# downloads (ADR-0067, ADR-0084; roadmap 09 phase 10). The container stands in
# for a fresh host; nothing on the machine running it changes.
#
# It checks what the unit tests cannot: that the real counterparties accept
# what the installer does.
#
#   1. preflight names the commands the host must supply, then the host's
#      own package manager supplies them (banlieue installs none);
#   2. a full install succeeds, then a second one changes nothing
#      (idempotence is equality: every path, mode, owner and digest);
#   3. the read-only verbs write nothing;
#   4. selftest passes (VMM, firmware pin, guest uids through NSS, /dev/kvm,
#      the provider's own host checks, a vTPM with the right EK CN);
#   5. a corrupt artifact installs nothing and leaves the release as it was;
#   6. a VMM older than the client's gate is refused before any download.
#
# Needs a rootful container runtime (the provider's user must open the
# host's /dev/kvm) and the banlieue binary to test:
#
#   BANLIEUE_BINARY=target/debug/banlieue scripts/test-ch-host-install.sh
set -euo pipefail

BANLIEUE_BINARY="${BANLIEUE_BINARY:?set BANLIEUE_BINARY to the banlieue binary to test}"
CONTAINER_RUNTIME="${CONTAINER_RUNTIME:-sudo podman}"
IMAGE="${CH_HOST_TEST_IMAGE:-docker.io/library/debian:trixie}"
# Optional: a directory holding cloud-hypervisor-static, ch-remote-static and
# CLOUDHV.fd, for a run without downloads. Empty: download the pinned release.
ARTIFACTS_DIR="${CH_HOST_ARTIFACTS_DIR:-}"

[[ -x "$BANLIEUE_BINARY" ]] || { echo "$BANLIEUE_BINARY is not an executable" >&2; exit 1; }
[[ -c /dev/kvm ]] || { echo "/dev/kvm is required" >&2; exit 1; }

mounts=(-v "$(realpath "$BANLIEUE_BINARY"):/usr/local/bin/banlieue-under-test:ro")
artifacts_flag=""
if [[ -n "$ARTIFACTS_DIR" ]]; then
  mounts+=(-v "$(realpath "$ARTIFACTS_DIR"):/artifacts:ro")
  artifacts_flag="--artifacts-dir /artifacts"
fi

# shellcheck disable=SC2086  # CONTAINER_RUNTIME may be `sudo podman`
$CONTAINER_RUNTIME run --rm --device /dev/kvm --cap-add NET_ADMIN "${mounts[@]}" \
  -e ARTIFACTS_FLAG="$artifacts_flag" "$IMAGE" bash -euo pipefail -c '
B=/usr/local/bin/banlieue-under-test
say() { echo; echo "######## $*"; }
# Every path the installer owns: path, mode, owner, and a file digest. The
# EK CA serial counter is left out: every vTPM manufactured advances it
# (the self-test too), and a CA must never reuse a serial.
snapshot() {
  find /etc/banlieue /etc/userdb /var/lib/banlieue /srv /opt/banlieue \
       /usr/local/bin /etc/systemd/system /etc/polkit-1/rules.d /etc/tmpfiles.d /run/banlieue \
       -xdev -printf "%p %m %u:%g %y %l\n" 2>/dev/null | sort
  find /etc/banlieue /etc/userdb /opt/banlieue /etc/systemd/system /etc/polkit-1/rules.d \
       /var/lib/banlieue/swtpm-localca -xdev -type f -exec sha256sum {} + 2>/dev/null \
    | grep -v "/swtpm-localca/certserial$" | sort
}

say "1. the host supplies its packages; banlieue installs none"
if $B host cloud-hypervisor preflight --allow-virtualized-host 2>/tmp/preflight; then
  echo "FAIL: preflight passed without swtpm"; exit 1
fi
grep -q "swtpm_setup is not on PATH" /tmp/preflight || { cat /tmp/preflight; echo "FAIL: preflight did not name swtpm_setup"; exit 1; }
echo "ok: preflight named what is missing"
# What an operator does, with the package manager of the host.
apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
  ca-certificates dbus iproute2 libnss-systemd polkitd swtpm swtpm-tools systemd >/dev/null
# A bridge for the network class; the installer never makes one.
ip link add br0 type bridge
# On a host, udev creates the kvm group and owns /dev/kvm by it. A
# container has no udev: stand in for it with the group id the device
# already carries, so the installer and preflight see what a host has.
getent group kvm >/dev/null || groupadd --system -g "$(stat -c %g /dev/kvm)" kvm

say "2. full install, from a host with no banlieue on it"
$B host cloud-hypervisor install --network-class default=br0 --provider-name ch-test $ARTIFACTS_FLAG
first=$(snapshot)
$B host cloud-hypervisor install --network-class default=br0 --provider-name ch-test $ARTIFACTS_FLAG
second=$(snapshot)
if [[ "$first" != "$second" ]]; then
  diff <(echo "$first") <(echo "$second") || true
  echo "FAIL: a second install changed the host"; exit 1
fi
echo "ok: a second install changed nothing ($(echo "$first" | wc -l) entries compared)"

say "3. the read-only verbs write nothing"
before=$(snapshot)
$B host cloud-hypervisor preflight --network-class default=br0
$B host cloud-hypervisor status
$B host cloud-hypervisor selftest --network-class default=br0
after=$(snapshot)
if [[ "$before" != "$after" ]]; then
  diff <(echo "$before") <(echo "$after") || true
  echo "FAIL: a read-only verb changed the host"; exit 1
fi
echo "ok: preflight, status and selftest changed nothing"

say "4. layout"
for p in /var/lib/banlieue:751:banlieue:banlieue /var/lib/banlieue/ek:700:banlieue:banlieue \
         /var/lib/banlieue/tpm:711:banlieue:banlieue /etc/banlieue/credentials:700:banlieue:banlieue \
         /etc/banlieue/cloud-hypervisor.toml:640:root:banlieue \
         /var/lib/banlieue/swtpm-localca/issuercert.pem:644:banlieue:banlieue; do
  IFS=: read -r path mode user group <<<"$p"
  got=$(stat -c "%a:%U:%G" "$path")
  [[ "$got" == "$mode:$user:$group" ]] || { echo "FAIL: $path is $got, want $mode:$user:$group"; exit 1; }
done
getent passwd 2000000 >/dev/null || { echo "FAIL: guest uid 2000000 does not resolve"; exit 1; }
echo "ok: modes, owners, guest uids"

say "5. a corrupt artifact installs nothing"
release=$(find /opt/banlieue /usr/local/bin -xdev -printf "%p %l\n" | sort; sha256sum /opt/banlieue/*/*/* | sort)
mkdir -p /bad && for f in cloud-hypervisor-static ch-remote-static CLOUDHV.fd; do echo tampered > /bad/$f; done
if $B host cloud-hypervisor install --only vmm --force --artifacts-dir /bad; then
  echo "FAIL: a corrupt artifact was accepted"; exit 1
fi
after=$(find /opt/banlieue /usr/local/bin -xdev -printf "%p %l\n" | sort; sha256sum /opt/banlieue/*/*/* | sort)
[[ "$release" == "$after" ]] || { echo "FAIL: the release changed"; exit 1; }
echo "ok: refused, and the installed release is byte-identical"

say "6. a VMM the provider would refuse is refused before any download"
if $B host cloud-hypervisor install --only vmm --vmm-version v52.0 2>/tmp/old; then
  echo "FAIL: v52.0 was accepted"; exit 1
fi
grep -q "older than" /tmp/old || { cat /tmp/old; echo "FAIL: wrong refusal"; exit 1; }
after=$(find /opt/banlieue /usr/local/bin -xdev -printf "%p %l\n" | sort; sha256sum /opt/banlieue/*/*/* | sort)
[[ "$release" == "$after" ]] || { echo "FAIL: the release changed"; exit 1; }
echo "ok: refused, nothing changed"

say "PASS"
'
