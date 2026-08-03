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

FORCE="${FORCE:-false}"

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
  local f="$1"
  [[ -f "$f" && "$FORCE" != "true" ]] && { log "$f exists, keeping (FORCE=true to regenerate)"; return 0; }
  return 1
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
detect_sans() {
  [[ -z "$SAN_DNS" ]] && SAN_DNS="$(hostname) $(hostname -f 2>/dev/null || true)"
  if [[ -z "$SAN_IPS" ]]; then
    if ! command -v ip >/dev/null 2>&1; then
      warn "\`ip\` not found: cannot auto-detect addresses (is this a Linux libvirt host?)."
      warn "Set SAN_IPS explicitly, e.g. SAN_IPS=\"192.0.2.10 192.0.2.1\""
      exit 1
    fi
    SAN_IPS="$(ip -4 -o addr show scope global | awk '{split($4,a,"/"); print a[1]}' | tr '\n' ' ')"
  fi
  [[ -n "${SAN_IPS// /}" ]] || { warn "no addresses detected; set SAN_IPS explicitly"; exit 1; }
}

# Emit the dns_name/ip_address SAN lines for the server template, deduplicated
# (`hostname` and `hostname -f` are identical on some hosts).
san_lines() {
  local n
  for n in $SAN_DNS; do [[ -n "$n" ]] && echo "dns_name = \"$n\""; done | sort -u
  for n in $SAN_IPS; do [[ -n "$n" ]] && echo "ip_address = \"$n\""; done | sort -u
}

make_server_cert() {
  keep_existing "$LIBVIRT_PKI/servercert.pem" && return 0
  detect_sans

  log "Generating server certificate (cn=$SERVER_CN)"
  log "  SAN dns: $SAN_DNS"
  log "  SAN ips: $SAN_IPS"
  certtool --generate-privkey > "$LIBVIRT_PKI/private/serverkey.pem" 2>/dev/null
  chmod 600 "$LIBVIRT_PKI/private/serverkey.pem"

  local tmpl; tmpl="$(mktemp)"
  {
    echo "organization = \"$ORG\""
    echo "cn = \"$SERVER_CN\""
    san_lines
    echo "expiration_days = $CERT_DAYS"
    echo "tls_www_server"
    echo "encryption_key"
    echo "signing_key"
  } >"$tmpl"

  certtool --generate-certificate \
    --load-privkey "$LIBVIRT_PKI/private/serverkey.pem" \
    --load-ca-certificate "$CA_DIR/cacert.pem" \
    --load-ca-privkey "$CA_DIR/cakey.pem" \
    --template "$tmpl" \
    --outfile "$LIBVIRT_PKI/servercert.pem" 2>/dev/null
  rm -f "$tmpl"
  chmod 644 "$LIBVIRT_PKI/servercert.pem"
}

make_client_cert() {
  keep_existing "$LIBVIRT_PKI/clientcert.pem" && return 0

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

main() {
  case "${1:-all}" in
    ca)        check_deps; make_ca ;;
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
      echo "Usage: $0 [all|ca|server|client|configure|verify|secret|status]" >&2
      exit 1
      ;;
  esac
}

main "$@"
