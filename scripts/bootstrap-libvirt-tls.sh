#!/usr/bin/env bash
# Provisions x509 PKI for libvirt and switches libvirtd from plaintext TCP to
# mutual-TLS, then emits a Kubernetes Secret manifest so banlieue's libvirt
# provider can authenticate with a client certificate.
#
# Run this ON the libvirt host (it writes /etc/pki and restarts libvirtd).
#
#   sudo ./scripts/bootstrap-libvirt-tls.sh all
#
# Every step is idempotent: existing keys/certs are kept unless FORCE=true, so
# re-running will not silently invalidate certificates already distributed to
# clients.
#
# Why this exists at all: libvirtd here was listening on plaintext TCP 16509
# with auth_tcp="sasl" / mech_list=digest-md5. DIGEST-MD5 was declared OBSOLETE
# by RFC 6331 and, absent a negotiated SASL security layer, the RPC session is
# unencrypted -- every VM definition, and the bytes of every uploaded disk
# image, in clear text on the wire. TLS with client certificates replaces both
# the encryption and the authentication story: with auth_tls="none" (libvirt's
# default) the client CERTIFICATE is the credential, so there is no shared
# secret and no MD5 anywhere.
#
# It also simplifies the client: banlieue then needs only TLS (rustls, already
# a workspace dependency) rather than a hand-rolled SASL DIGEST-MD5 exchange.
#
# SANs: libvirt validates the server certificate against the address the CLIENT
# connected to. This host is reachable on several (LAN, tailnet, and the
# libvirt bridge that in-cluster pods actually use), so every one of them is
# baked in -- a cert covering only the hostname fails with an opaque TLS error
# the moment something connects by IP. (The same class of mistake as an API
# server cert missing a SAN.)
set -euo pipefail

# Identity baked into the certificates.
CA_CN="${CA_CN:-banlieue libvirt CA}"
ORG="${ORG:-banlieue}"
SERVER_CN="${SERVER_CN:-$(hostname -f 2>/dev/null || hostname)}"
CLIENT_CN="${CLIENT_CN:-banlieue-provider-libvirt}"

# Every name/address a client might connect to. Left EMPTY here and resolved
# lazily by detect_sans() only when a server certificate is actually being
# generated -- auto-detecting at load time would run `ip`, which does not exist
# on non-Linux hosts, and under `set -e` that kills the script before it can so
# much as print its usage (a bare exit 127, no message).
SAN_DNS="${SAN_DNS:-}"
SAN_IPS="${SAN_IPS:-}"

# libvirt's documented, hard-coded lookup paths.
CA_DIR="${CA_DIR:-/etc/pki/CA}"
LIBVIRT_PKI="${LIBVIRT_PKI:-/etc/pki/libvirt}"
CERT_DAYS="${CERT_DAYS:-3650}"

# Turn the plaintext listener off once TLS works. Set false to run both during
# a migration.
DISABLE_TCP="${DISABLE_TCP:-true}"
LIBVIRTD_CONF="${LIBVIRTD_CONF:-/etc/libvirt/libvirtd.conf}"

# Where to write the Kubernetes Secret manifest for the client credentials.
SECRET_OUT="${SECRET_OUT:-./libvirt-client-tls-secret.yaml}"
SECRET_NAME="${SECRET_NAME:-libvirt-client-creds}"
SECRET_NAMESPACE="${SECRET_NAMESPACE:-banlieue-system}"

# FORCE regenerates EVERYTHING, the CA included -- which invalidates every
# client certificate already handed out. FORCE_SERVER reissues only the server
# certificate, leaving the CA and all client certs alone.
#
# That distinction matters because "add a SAN" is by far the most common
# reason to re-run this (a host joins a tailnet, an address changes), and
# before FORCE_SERVER existed the only lever also rotated the CA -- so the
# cheap, routine fix carried the most expensive possible side effect.
FORCE="${FORCE:-false}"
FORCE_SERVER="${FORCE_SERVER:-false}"

log()  { echo "==> $*" >&2; }
warn() { echo "!!! $*" >&2; }

check_deps() {
  command -v certtool >/dev/null 2>&1 || {
    warn "certtool not found (Debian/Ubuntu: apt install gnutls-bin)"; exit 1; }
  [[ $EUID -eq 0 ]] || { warn "must run as root (writes $CA_DIR and restarts libvirtd)"; exit 1; }
}

# Skip regeneration unless FORCE -- reissuing a CA silently invalidates every
# client certificate already handed out.
keep_existing() {
  local f="$1" force="${2:-$FORCE}"
  [[ -f "$f" && "$force" != "true" ]] && { log "$f exists, keeping (FORCE=true to regenerate)"; return 0; }
  return 1
}

# True when the server certificate should be reissued. Deliberately separate
# from the CA: `$0 server` calls make_ca only to ENSURE a CA exists, so FORCE
# must not reach it and rotate the thing every client trusts.
force_server() {
  [[ "$FORCE" == "true" || "$FORCE_SERVER" == "true" ]] && echo true || echo false
}

make_ca() {
  mkdir -p "$CA_DIR" "$LIBVIRT_PKI/private"
  chmod 700 "$LIBVIRT_PKI/private"
  keep_existing "$CA_DIR/cacert.pem" && return 0

  log "Generating CA key + self-signed certificate ($CA_CN)"
  certtool --generate-privkey > "$CA_DIR/cakey.pem" 2>/dev/null
  chmod 600 "$CA_DIR/cakey.pem"

  local tmpl; tmpl="$(mktemp)"
  cat >"$tmpl" <<EOF
cn = "$CA_CN"
organization = "$ORG"
expiration_days = $CERT_DAYS
ca
cert_signing_key
EOF
  certtool --generate-self-signed \
    --load-privkey "$CA_DIR/cakey.pem" \
    --template "$tmpl" \
    --outfile "$CA_DIR/cacert.pem" 2>/dev/null
  rm -f "$tmpl"
  chmod 644 "$CA_DIR/cacert.pem"
}

# Resolve the names/addresses to cover, unless the caller supplied them.
# Deliberately includes EVERY global IPv4 address, notably the libvirt bridge
# (virbr0): that is how workloads inside guest VMs reach the host, and it is
# the address most easily forgotten. libvirt validates the server certificate
# against whatever the client dialled, so a cert covering only the hostname
# fails with an opaque TLS error the moment anything connects by IP.
# The host's tailnet identity, taken from tailscale itself rather than from
# interface enumeration.
#
# Both are needed and neither is reliably found by `ip addr`. The MagicDNS
# name is not `hostname -f` and appears on no interface at all, so a
# tailnet-connected host ends up with a certificate covering its tailnet
# ADDRESS but not the NAME clients naturally dial -- which fails as
# `certificate not valid for name ...`, the opaque error this script's header
# warns about.
#
# It is also the most durable of the three identities: a LAN address is DHCP
# and moves, `hostname -f` depends on local resolver config, but a tailnet
# name and its 100.64/10 address are assigned by the tailnet and stay put.
tailnet_dns() {
  command -v tailscale >/dev/null 2>&1 || return 0
  command -v python3   >/dev/null 2>&1 || return 0
  tailscale status --json 2>/dev/null | python3 -c '
import json, sys
try:
    print(json.load(sys.stdin).get("Self", {}).get("DNSName", "").rstrip("."))
except Exception:
    pass' 2>/dev/null
}

tailnet_ip() {
  command -v tailscale >/dev/null 2>&1 || return 0
  tailscale ip -4 2>/dev/null | head -1
}

detect_sans() {
  local ts_dns ts_ip
  ts_dns="$(tailnet_dns)"
  ts_ip="$(tailnet_ip)"

  # Running on a tailnet but unable to read the name is the one case that
  # would silently reproduce the bug this function exists to prevent.
  if command -v tailscale >/dev/null 2>&1 && [[ -z "$ts_dns" ]]; then
    warn "tailscale is installed but its MagicDNS name could not be read."
    warn "  The certificate will NOT cover <host>.<tailnet>.ts.net."
    warn "  Pass it explicitly, e.g. SAN_DNS=\"\$(hostname) \$(hostname -f) host.tailnet.ts.net\""
  fi

  [[ -z "$SAN_DNS" ]] && SAN_DNS="$(hostname) $(hostname -f 2>/dev/null || true) $ts_dns"
  if [[ -z "$SAN_IPS" ]]; then
    if ! command -v ip >/dev/null 2>&1; then
      warn "\`ip\` not found: cannot auto-detect addresses (is this a Linux libvirt host?)."
      warn "Set SAN_IPS explicitly, e.g. SAN_IPS=\"192.0.2.10 192.0.2.1\""
      exit 1
    fi
    SAN_IPS="$(ip -4 -o addr show scope global | awk '{split($4,a,"/"); print a[1]}' | tr '\n' ' ')"
    # Belt and braces: the tailnet address is normally scope-global and so is
    # already in that list, but it is the one address whose absence is worst
    # (it is how anything off-LAN reaches this host). san_lines() dedupes.
    SAN_IPS="$SAN_IPS $ts_ip"
  fi
  [[ -n "${SAN_IPS// /}" ]] || { warn "no addresses detected; set SAN_IPS explicitly"; exit 1; }

  # Normalise so what gets logged is exactly what gets issued. san_lines()
  # sorts and dedupes for the template anyway; without this the log shows
  # duplicates and readers reasonably wonder which list is real.
  SAN_DNS="$(echo "$SAN_DNS" | tr ' ' '\n' | grep -v '^$' | sort -u | tr '\n' ' ')"
  SAN_IPS="$(echo "$SAN_IPS" | tr ' ' '\n' | grep -v '^$' | sort -u | tr '\n' ' ')"
}

# Emit the dns_name/ip_address SAN lines for the server template, deduplicated
# (`hostname` and `hostname -f` are identical on some hosts).
san_lines() {
  local n
  for n in $SAN_DNS; do [[ -n "$n" ]] && echo "dns_name = \"$n\""; done | sort -u
  for n in $SAN_IPS; do [[ -n "$n" ]] && echo "ip_address = \"$n\""; done | sort -u
}

make_server_cert() {
  keep_existing "$LIBVIRT_PKI/servercert.pem" "$(force_server)" && return 0

  # Signing needs the CA PRIVATE key, which on a multi-host setup lives on
  # exactly one machine. Without this check certtool fails into 2>/dev/null
  # below and leaves a missing or truncated certificate behind -- a silent
  # half-failure that surfaces later as a TLS handshake error on a host
  # nobody was touching.
  if [[ ! -f "$CA_DIR/cakey.pem" ]]; then
    warn "No CA private key at $CA_DIR/cakey.pem -- cannot sign a server certificate here."
    warn "  This host trusts the CA (cacert.pem) but does not hold it. Either:"
    warn "    1. run this on the CA host with SERVER_CN/SAN_DNS/SAN_IPS set for THIS"
    warn "       host, then copy servercert.pem + private/serverkey.pem back, or"
    warn "    2. copy cakey.pem here temporarily, re-run, and shred it afterwards."
    warn "  Do not generate a new CA: every client certificate already issued"
    warn "  chains to the existing one and would stop working."
    exit 1
  fi

  detect_sans

  log "Generating server certificate (cn=$SERVER_CN)"
  log "  SAN dns: $SAN_DNS"
  log "  SAN ips: $SAN_IPS"

  # Build the new key and certificate OUT OF PLACE, and install both only
  # once both exist. Writing serverkey.pem first and signing afterwards --
  # the obvious order -- leaves a new key beside the OLD certificate if the
  # signature fails, and libvirtd then refuses to start TLS at all. Reissuing
  # a certificate must never be able to take the host offline.
  local newkey newcert tmpl
  newkey="$(mktemp)"; newcert="$(mktemp)"; tmpl="$(mktemp)"
  # shellcheck disable=SC2064  # expand now: these paths must not change
  trap "rm -f '$newkey' '$newcert' '$tmpl'" RETURN

  certtool --generate-privkey > "$newkey" 2>/dev/null
  chmod 600 "$newkey"

  {
    echo "organization = \"$ORG\""
    echo "cn = \"$SERVER_CN\""
    san_lines
    echo "expiration_days = $CERT_DAYS"
    echo "tls_www_server"
    echo "encryption_key"
    echo "signing_key"
  } >"$tmpl"

  # Errors are NOT swallowed: a failed signature must not look like success.
  if ! certtool --generate-certificate \
    --load-privkey "$newkey" \
    --load-ca-certificate "$CA_DIR/cacert.pem" \
    --load-ca-privkey "$CA_DIR/cakey.pem" \
    --template "$tmpl" \
    --outfile "$newcert"; then
    warn "certtool failed to sign the server certificate — nothing was changed"
    exit 1
  fi
  [[ -s "$newcert" ]] || { warn "signed certificate is empty — nothing was changed"; exit 1; }

  # Keep the outgoing pair until the new one is in place, so a bad reissue
  # can be undone by hand.
  if [[ -f "$LIBVIRT_PKI/servercert.pem" ]]; then
    cp -p "$LIBVIRT_PKI/servercert.pem" "$LIBVIRT_PKI/servercert.pem.prev"
    cp -p "$LIBVIRT_PKI/private/serverkey.pem" "$LIBVIRT_PKI/private/serverkey.pem.prev" 2>/dev/null || true
    log "  previous pair saved as *.prev"
  fi

  install -m600 "$newkey" "$LIBVIRT_PKI/private/serverkey.pem"
  install -m644 "$newcert" "$LIBVIRT_PKI/servercert.pem"

  log "  Issued. Restart libvirtd for it to take effect:"
  log "    sudo systemctl restart libvirtd"
}

# ---------------------------------------------------------------------------
# Two-host flow: certify a host that does NOT hold the CA private key.
#
# The alternative people reach for is copying cakey.pem to the other host, or
# copying a freshly-made server key back from the CA host. Both move a private
# key across the network; the first moves the ONE key that compromises every
# certificate this CA will ever issue. With a CSR nothing secret moves at all:
# the server key is generated on the host that will use it and never leaves,
# and only a signing request and a public certificate cross the wire.
# ---------------------------------------------------------------------------
CSR_OUT="${CSR_OUT:-/tmp/banlieue-server.csr}"
CERT_OUT="${CERT_OUT:-/tmp/banlieue-server.pem}"

server_template() {
  echo "organization = \"$ORG\""
  echo "cn = \"$SERVER_CN\""
  san_lines
  echo "expiration_days = $CERT_DAYS"
  echo "tls_www_server"
  echo "encryption_key"
  echo "signing_key"
}

# Run on the host that NEEDS a certificate.
make_server_csr() {
  mkdir -p "$LIBVIRT_PKI/private"; chmod 700 "$LIBVIRT_PKI/private"
  detect_sans

  local key="$LIBVIRT_PKI/private/serverkey.pem"
  if [[ ! -f "$key" || "$(force_server)" == "true" ]]; then
    log "Generating server private key (stays on this host)"
    certtool --generate-privkey > "$key" 2>/dev/null
    chmod 600 "$key"
  else
    log "$key exists, reusing (FORCE_SERVER=true to regenerate)"
  fi

  local tmpl; tmpl="$(mktemp)"
  server_template >"$tmpl"
  if ! certtool --generate-request --load-privkey "$key" \
       --template "$tmpl" --outfile "$CSR_OUT"; then
    rm -f "$tmpl"; warn "certtool failed to generate the request"; exit 1
  fi
  rm -f "$tmpl"

  log "Request written to $CSR_OUT"
  log ""
  log "On the CA host, sign it with these SANs (they describe THIS host):"
  log "  scp $CSR_OUT <ca-host>:/tmp/"
  log "  sudo SERVER_CN=\"$SERVER_CN\" \\"
  log "       SAN_DNS=\"$SAN_DNS\" \\"
  log "       SAN_IPS=\"$SAN_IPS\" \\"
  log "       CSR_IN=$CSR_OUT $0 sign"
  log "  # then copy $CERT_OUT back and install it:"
  log "  sudo install -m644 $CERT_OUT $LIBVIRT_PKI/servercert.pem"
  log "  sudo systemctl restart libvirtd"
}

# Run on the CA host. SANs are NOT read from the request: certtool takes them
# from the template, so they must be passed explicitly -- which is why
# `csr` prints the exact command above rather than leaving it to be retyped.
sign_csr() {
  local csr="${CSR_IN:-$CSR_OUT}"
  [[ -f "$csr" ]] || { warn "no request at $csr (set CSR_IN)"; exit 1; }
  [[ -f "$CA_DIR/cakey.pem" ]] || { warn "no CA private key at $CA_DIR/cakey.pem -- this is not the CA host"; exit 1; }
  [[ -n "$SAN_DNS" && -n "$SAN_IPS" ]] || {
    warn "set SAN_DNS and SAN_IPS explicitly: they describe the REQUESTING host,"
    warn "not this one, so auto-detection would certify the wrong machine."
    exit 1; }

  log "Signing $csr for cn=$SERVER_CN"
  log "  SAN dns: $SAN_DNS"
  log "  SAN ips: $SAN_IPS"
  local tmpl; tmpl="$(mktemp)"
  server_template >"$tmpl"
  if ! certtool --generate-certificate --load-request "$csr" \
       --load-ca-certificate "$CA_DIR/cacert.pem" \
       --load-ca-privkey "$CA_DIR/cakey.pem" \
       --template "$tmpl" --outfile "$CERT_OUT"; then
    rm -f "$tmpl"; warn "certtool failed to sign the request"; exit 1
  fi
  rm -f "$tmpl"
  [[ -s "$CERT_OUT" ]] || { warn "signed certificate is empty"; exit 1; }
  chmod 644 "$CERT_OUT"
  log "Signed certificate at $CERT_OUT -- copy it back to the requesting host."
}

make_client_cert() {
  keep_existing "$LIBVIRT_PKI/clientcert.pem" "$FORCE" && return 0

  log "Generating client certificate (cn=$CLIENT_CN)"
  certtool --generate-privkey > "$LIBVIRT_PKI/private/clientkey.pem" 2>/dev/null
  chmod 600 "$LIBVIRT_PKI/private/clientkey.pem"

  local tmpl; tmpl="$(mktemp)"
  cat >"$tmpl" <<EOF
country = "CA"
organization = "$ORG"
cn = "$CLIENT_CN"
expiration_days = $CERT_DAYS
tls_www_client
encryption_key
signing_key
EOF
  certtool --generate-certificate \
    --load-privkey "$LIBVIRT_PKI/private/clientkey.pem" \
    --load-ca-certificate "$CA_DIR/cacert.pem" \
    --load-ca-privkey "$CA_DIR/cakey.pem" \
    --template "$tmpl" \
    --outfile "$LIBVIRT_PKI/clientcert.pem" 2>/dev/null
  rm -f "$tmpl"
  chmod 644 "$LIBVIRT_PKI/clientcert.pem"
}

# Set `key = value` in libvirtd.conf, editing an existing ACTIVE setting or
# appending a new one.
#
# The patterns deliberately do NOT match commented lines. libvirtd.conf
# documents every option as a commented example (`#listen_tls = 0`) far above
# the file's real settings, so a `#?` in the pattern matches the documentation
# too and *uncomments* it -- silently turning prose into configuration and
# leaving two active copies of the same key. Harmless when both copies happen
# to get the same value, actively dangerous when they don't.
set_conf() {
  local key="$1" val="$2"
  if grep -qE "^[[:space:]]*${key}[[:space:]]*=" "$LIBVIRTD_CONF"; then
    sed -i -E "s|^[[:space:]]*${key}[[:space:]]*=.*|${key} = ${val}|" "$LIBVIRTD_CONF"
  else
    echo "${key} = ${val}" >>"$LIBVIRTD_CONF"
  fi
}

configure_libvirtd() {
  log "Configuring $LIBVIRTD_CONF for TLS"
  cp -n "$LIBVIRTD_CONF" "${LIBVIRTD_CONF}.pre-banlieue-tls" 2>/dev/null || true

  set_conf listen_tls 1
  # auth_tls defaults to "none", which means the CLIENT CERTIFICATE is the
  # credential -- x509 mutual TLS, no shared secret. Stated explicitly so the
  # security model is visible in the config rather than implied by a default.
  set_conf auth_tls '"none"'

  if [[ "$DISABLE_TCP" == "true" ]]; then
    set_conf listen_tcp 0
  else
    warn "DISABLE_TCP=false -- plaintext 16509 stays enabled alongside TLS"
  fi

  # Socket-activation ordering matters and is easy to get wrong:
  # systemd REFUSES to start a .socket whose service is already running --
  #   "Socket service libvirtd.service already active, refusing."
  # A long-running libvirtd (they typically have months of uptime) therefore
  # makes a naive `systemctl enable --now libvirtd-tls.socket` fail every time.
  # The daemon also keeps serving whatever socket fds it already inherited, so
  # merely disabling libvirtd-tcp.socket does NOT close port 16509 on a running
  # process. Both problems have the same fix: stop the service and all its
  # sockets first, change what is enabled, then bring them back up together.
  local socks=(libvirtd.socket libvirtd-ro.socket libvirtd-admin.socket
               libvirtd-tls.socket libvirtd-tcp.socket)

  log "Stopping libvirtd and its sockets to re-arm socket activation"
  systemctl stop libvirtd.service >/dev/null 2>&1 || true
  systemctl stop "${socks[@]}" >/dev/null 2>&1 || true

  if [[ "$DISABLE_TCP" == "true" ]]; then
    log "Disabling the plaintext TCP listener"
    systemctl disable libvirtd-tcp.socket >/dev/null 2>&1 || true
  fi

  log "Enabling the TLS socket"
  systemctl enable libvirtd-tls.socket >/dev/null 2>&1 || true

  # Start the sockets first so the service inherits the right set of fds.
  local want=(libvirtd.socket libvirtd-ro.socket libvirtd-admin.socket libvirtd-tls.socket)
  [[ "$DISABLE_TCP" == "true" ]] || want+=(libvirtd-tcp.socket)
  if ! systemctl start "${want[@]}"; then
    warn "failed to start libvirt sockets; check: systemctl status libvirtd-tls.socket"
    exit 1
  fi

  if ! systemctl start libvirtd.service; then
    warn "libvirtd failed to start; check: journalctl -u libvirtd -n 50"
    exit 1
  fi
  sleep 2
}

verify() {
  log "Verifying"
  local ok=0
  if ss -lntp 2>/dev/null | grep -q ':16514'; then
    log "  TLS listener active on 16514"
  else
    warn "  no listener on 16514"; ok=1
  fi
  if [[ "$DISABLE_TCP" == "true" ]]; then
    if ss -lntp 2>/dev/null | grep -q ':16509'; then
      warn "  plaintext 16509 is STILL listening"; ok=1
    else
      log "  plaintext 16509 is closed"
    fi
  fi
  # A local round-trip proves cert chain + SANs + libvirtd config together.
  if virsh -c "qemu+tls://$SERVER_CN/system" version >/dev/null 2>&1; then
    log "  qemu+tls://$SERVER_CN/system connects"
  else
    warn "  could not connect over TLS; try:"
    warn "    virsh -c qemu+tls://$SERVER_CN/system version"
    ok=1
  fi
  return $ok
}

# The provider consumes these through the API that already exists:
# connection.caBundle (ADR-0008, secretRef) and connection.credentialsRef.
write_secret() {
  log "Writing Secret manifest to $SECRET_OUT"
  local ca crt key
  ca="$(base64 -w0 <"$CA_DIR/cacert.pem")"
  crt="$(base64 -w0 <"$LIBVIRT_PKI/clientcert.pem")"
  key="$(base64 -w0 <"$LIBVIRT_PKI/private/clientkey.pem")"
  cat >"$SECRET_OUT" <<EOF
# Client credentials for banlieue's libvirt provider (mutual TLS).
# Generated by scripts/bootstrap-libvirt-tls.sh -- contains a PRIVATE KEY.
# Do not commit. Apply with:
#   kubectl apply -f $(basename "$SECRET_OUT")
apiVersion: v1
kind: Secret
metadata:
  name: $SECRET_NAME
  namespace: $SECRET_NAMESPACE
type: Opaque
data:
  ca.crt: $ca
  tls.crt: $crt
  tls.key: $key
EOF
  chmod 600 "$SECRET_OUT"
  log "Secret written. It contains a private key -- move it securely, do not commit."
}

status() {
  echo "--- listeners ---"; ss -lntp 2>/dev/null | grep -E ':1650[0-9]|:1651[0-9]' || echo "(none)"
  echo "--- config ---";    grep -hE '^\s*(listen_tls|listen_tcp|auth_tls|auth_tcp)' "$LIBVIRTD_CONF" 2>/dev/null || echo "(defaults)"
  echo "--- certs ---"
  for f in "$CA_DIR/cacert.pem" "$LIBVIRT_PKI/servercert.pem" "$LIBVIRT_PKI/clientcert.pem"; do
    [[ -f "$f" ]] && echo "$f: $(certtool -i --infile "$f" 2>/dev/null | grep -E 'Subject:|Not After:' | tr '\n' ' ')" || echo "$f: MISSING"
  done
}

# Read-only: print exactly the SANs a server certificate would be issued
# with, and how they compare to the one already installed.
#
# Worth having because the failure this script guards against is silent
# until a client dials the missing name, by which point the certificate is
# already deployed and a restart away from being noticed.
show_sans() {
  detect_sans
  echo "would issue cn=$SERVER_CN with:"
  echo "  SAN dns: $SAN_DNS"
  echo "  SAN ips: $SAN_IPS"
  local cert="$LIBVIRT_PKI/servercert.pem"
  if [[ -f "$cert" ]] && command -v openssl >/dev/null 2>&1; then
    echo "currently installed:"
    openssl x509 -in "$cert" -noout -ext subjectAltName 2>/dev/null \
      | tail -n +2 | sed 's/^ */  /'
  fi
}

main() {
  case "${1:-all}" in
    ca)        check_deps; make_ca ;;
    sans)      show_sans ;;
    csr)       check_deps; make_server_csr ;;
    sign)      check_deps; sign_csr ;;
    server)    check_deps; make_ca; make_server_cert ;;
    client)    check_deps; make_ca; make_client_cert ;;
    configure) check_deps; configure_libvirtd; verify ;;
    secret)    write_secret ;;
    verify)    verify ;;
    status)    status ;;
    all)
      check_deps
      make_ca
      make_server_cert
      make_client_cert
      configure_libvirtd
      verify
      write_secret
      ;;
    *)
      echo "Usage: $0 [all|ca|server|client|configure|verify|secret|status|sans|csr|sign]" >&2
      echo "" >&2
      echo "Env:" >&2
      echo "  FORCE=true         regenerate EVERYTHING, CA included (invalidates all client certs)" >&2
      echo "  FORCE_SERVER=true  reissue only the server certificate (safe: CA and clients untouched)" >&2
      echo "  SAN_DNS / SAN_IPS  override SAN auto-detection entirely" >&2
      echo "" >&2
      echo "  $0 sans            show what SANs would be used, change nothing" >&2
      echo "" >&2
      echo "To add a SAN on a host that HOLDS the CA key:" >&2
      echo "  sudo FORCE_SERVER=true $0 server && sudo systemctl restart libvirtd" >&2
      echo "" >&2
      echo "On a host that does NOT hold the CA key, no private key may move:" >&2
      echo "  sudo FORCE_SERVER=true $0 csr    # here; prints the sign command" >&2
      echo "  ... sign on the CA host, copy the certificate back ..." >&2
      exit 1
      ;;
  esac
}

main "$@"
