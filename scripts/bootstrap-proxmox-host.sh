#!/usr/bin/env bash
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
#
# Prepares a Proxmox VE node as a target for banlieue's Proxmox provider
# (ADR-0074): a least-privilege role and API token, a pveproxy certificate
# that names every address a client connects by, and an optional cloud-image
# template for `ProxmoxMachine` clones.
#
# Run this ON the Proxmox node, as root:
#
#   ./scripts/bootstrap-proxmox-host.sh all
#   ssh root@bar.foo.io 'bash -s -- all' < scripts/bootstrap-proxmox-host.sh
#
# Steps are idempotent: an existing role is re-synced, an existing token is
# kept (its secret cannot be read back, so re-issuing needs FORCE_TOKEN=true),
# an existing custom certificate is kept unless FORCE_CERT=true, and an
# existing template VMID is left alone.
#
# Why a custom pveproxy certificate: the one Proxmox generates names only the
# short hostname, its FQDN and the node's LAN addresses. banlieue verifies the
# server certificate against the endpoint it connects to, so reaching the node
# over any other name (a tailnet name, a load-balancer name) fails with an
# opaque TLS error. The replacement is signed by the node's own PVE root CA,
# so the provider trusts exactly one CA (`/etc/pve/pve-root-ca.pem`, supplied
# as the Provider's `caBundle`) and nothing about the cluster's own
# certificates changes.
set -euo pipefail

PVE_USER="${PVE_USER:-banlieue@pve}"
TOKEN_ID="${TOKEN_ID:-provider}"
ROLE="${ROLE:-BanlieueProvider}"
NODE="${NODE:-$(hostname)}"

# Storage the provider may allocate disks on, and the storage it uploads
# NoCloud seed ISOs to (must carry `iso` content). Space separated.
IMAGE_STORAGES="${IMAGE_STORAGES:-local-lvm}"
ISO_STORAGE="${ISO_STORAGE:-local}"
# SDN zone holding the bridges VMs attach to. `localnetwork` is the implicit
# zone every plain Linux bridge (vmbr0, ...) belongs to.
SDN_ZONE="${SDN_ZONE:-localnetwork}"

# Extra SANs for the pveproxy certificate, space separated. The hostname, its
# FQDN and every global address on the node are always included.
EXTRA_SAN_DNS="${EXTRA_SAN_DNS:-}"
FORCE_CERT="${FORCE_CERT:-false}"
FORCE_TOKEN="${FORCE_TOKEN:-false}"
CERT_DAYS="${CERT_DAYS:-730}"

# Template: a cloud image imported as a Proxmox template, the clone source a
# ProxmoxMachine names by VMID.
TEMPLATE_VMID="${TEMPLATE_VMID:-9000}"
TEMPLATE_NAME="${TEMPLATE_NAME:-debian-13-genericcloud}"
TEMPLATE_URL="${TEMPLATE_URL:-https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-amd64.qcow2}"
TEMPLATE_BRIDGE="${TEMPLATE_BRIDGE:-vmbr0}"

SECRET_OUT="${SECRET_OUT:-./proxmox-provider-token-secret.yaml}"
SECRET_NAME="${SECRET_NAME:-proxmox-creds}"
CA_OUT="${CA_OUT:-./proxmox-ca.pem}"

# Everything the provider does, and nothing else. Deliberately absent:
# VM.Console, VM.GuestAgent.{FileRead,FileWrite,Unrestricted} (guest exec),
# VM.Migrate, VM.Snapshot*, VM.Backup, Sys.Modify, Permissions.Modify.
ROLE_PRIVS="VM.Allocate,VM.Audit,VM.Clone,VM.Config.CDROM,VM.Config.CPU,VM.Config.Cloudinit,VM.Config.Disk,VM.Config.HWType,VM.Config.Memory,VM.Config.Network,VM.Config.Options,VM.PowerMgmt,VM.GuestAgent.Audit,Datastore.AllocateSpace,Datastore.AllocateTemplate,Datastore.Audit,SDN.Audit,SDN.Use,Sys.Audit"

log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

require_pve() {
  command -v pveum >/dev/null 2>&1 || die "pveum not found; run this on a Proxmox VE node"
  [ "$(id -u)" -eq 0 ] || die "must run as root"
}

acl_paths() {
  echo /vms "/nodes/${NODE}" "/sdn/zones/${SDN_ZONE}" "/storage/${ISO_STORAGE}"
  for s in ${IMAGE_STORAGES}; do echo "/storage/${s}"; done
}

step_role() {
  if pveum role list --output-format json | grep -q "\"roleid\":\"${ROLE}\""; then
    log "role ${ROLE}: re-syncing privileges"
    pveum role modify "${ROLE}" --privs "${ROLE_PRIVS}"
  else
    log "role ${ROLE}: creating"
    pveum role add "${ROLE}" --privs "${ROLE_PRIVS}"
  fi
}

step_token() {
  if ! pveum user list --output-format json | grep -q "\"userid\":\"${PVE_USER}\""; then
    log "user ${PVE_USER}: creating (no password; API token only)"
    pveum user add "${PVE_USER}" --comment "banlieue Proxmox provider"
  fi

  local full="${PVE_USER}!${TOKEN_ID}"
  if pveum user token list "${PVE_USER}" --output-format json | grep -q "\"tokenid\":\"${TOKEN_ID}\""; then
    if [ "${FORCE_TOKEN}" != "true" ]; then
      log "token ${full}: exists, keeping it (secret is not retrievable; FORCE_TOKEN=true re-issues)"
      grant_acls "${full}"
      return
    fi
    log "token ${full}: re-issuing"
    pveum user token remove "${PVE_USER}" "${TOKEN_ID}"
  fi

  # Privilege-separated: the token holds only the ACLs granted to it below,
  # never the user's own.
  local json secret
  json="$(pveum user token add "${PVE_USER}" "${TOKEN_ID}" --privsep 1 \
    --comment "banlieue provider" --output-format json)"
  secret="$(printf '%s' "${json}" | sed -n 's/.*"value":"\([^"]*\)".*/\1/p')"
  [ -n "${secret}" ] || die "could not parse token secret from pveum output"

  grant_acls "${full}"

  (
    umask 077
    cat >"${SECRET_OUT}" <<EOF
apiVersion: v1
kind: Secret
metadata:
  name: ${SECRET_NAME}
type: Opaque
stringData:
  username: "${full}"
  tokenValue: "${secret}"
EOF
  )
  log "token ${full}: Secret manifest written to ${SECRET_OUT} (mode 0600)"
}

grant_acls() {
  local full="$1"
  for p in $(acl_paths); do
    pveum acl modify "${p}" --tokens "${full}" --roles "${ROLE}"
    # The token's effective privileges are the intersection of its own and
    # its user's, so the user needs the same grant.
    pveum acl modify "${p}" --users "${PVE_USER}" --roles "${ROLE}"
  done
  log "ACLs: ${ROLE} on $(acl_paths | tr '\n' ' ')"
}

detect_sans() {
  local dns ips
  dns="$(hostname) $(hostname -f 2>/dev/null || true) localhost ${EXTRA_SAN_DNS}"
  # A tailnet name, when the node is on one: it is how off-LAN dev clusters
  # reach the node, and it is not in /etc/hosts.
  if command -v tailscale >/dev/null 2>&1; then
    dns="${dns} $(tailscale status --json 2>/dev/null \
      | sed -n 's/.*"DNSName": *"\([^"]*\)\.".*/\1/p' | head -n 1)"
  fi
  # Global-scope addresses only: link-local ones are never a connect target.
  ips="127.0.0.1 ::1 $(ip -o addr show scope global | awk '{print $4}' | cut -d/ -f1 | tr '\n' ' ')"
  local out="" seen=" "
  for d in ${dns}; do
    case "${seen}" in *" DNS:${d} "*) continue ;; esac
    seen="${seen}DNS:${d} "; out="${out:+${out},}DNS:${d}"
  done
  for i in ${ips}; do
    case "${seen}" in *" IP:${i} "*) continue ;; esac
    seen="${seen}IP:${i} "; out="${out:+${out},}IP:${i}"
  done
  echo "${out}"
}

step_cert() {
  local dir=/etc/pve/local
  if [ -f "${dir}/pveproxy-ssl.pem" ] && [ "${FORCE_CERT}" != "true" ]; then
    log "cert: custom pveproxy certificate exists, keeping it (FORCE_CERT=true replaces)"
  else
    local sans work
    sans="$(detect_sans)"
    work="$(mktemp -d)"
    trap 'rm -rf "${work}"' RETURN
    cat >"${work}/req.cnf" <<EOF
[req]
distinguished_name=dn
prompt=no
[dn]
CN=$(hostname -f 2>/dev/null || hostname)
O=Proxmox Virtual Environment
OU=PVE Cluster Node
[ext]
basicConstraints=CA:FALSE
keyUsage=digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=${sans}
EOF
    log "cert: issuing pveproxy certificate for ${sans}"
    openssl req -new -newkey rsa:2048 -nodes -keyout "${work}/key.pem" \
      -out "${work}/req.csr" -config "${work}/req.cnf" 2>/dev/null
    openssl x509 -req -in "${work}/req.csr" -CA /etc/pve/pve-root-ca.pem \
      -CAkey /etc/pve/priv/pve-root-ca.key -CAcreateserial -days "${CERT_DAYS}" \
      -sha256 -extfile "${work}/req.cnf" -extensions ext -out "${work}/cert.pem" 2>/dev/null
    cp "${work}/key.pem" "${dir}/pveproxy-ssl.key"
    cp "${work}/cert.pem" "${dir}/pveproxy-ssl.pem"
    systemctl restart pveproxy
  fi
  cp /etc/pve/pve-root-ca.pem "${CA_OUT}"
  log "cert: CA bundle for the Provider's caBundle written to ${CA_OUT}"
}

step_template() {
  if qm status "${TEMPLATE_VMID}" >/dev/null 2>&1; then
    log "template: VMID ${TEMPLATE_VMID} exists, leaving it alone"
    return
  fi
  local storage file
  storage="$(echo "${IMAGE_STORAGES}" | awk '{print $1}')"
  file="/var/lib/vz/import/${TEMPLATE_NAME}.qcow2"
  mkdir -p /var/lib/vz/import
  if [ ! -f "${file}" ]; then
    log "template: downloading ${TEMPLATE_URL}"
    curl -fsSL -o "${file}.part" "${TEMPLATE_URL}"
    mv "${file}.part" "${file}"
  fi
  log "template: creating VMID ${TEMPLATE_VMID} (${TEMPLATE_NAME}) on ${storage}"
  qm create "${TEMPLATE_VMID}" --name "${TEMPLATE_NAME}" --ostype l26 \
    --machine q35 --cpu host --cores 2 --memory 2048 \
    --net0 "virtio,bridge=${TEMPLATE_BRIDGE}" \
    --scsihw virtio-scsi-single \
    --scsi0 "${storage}:0,import-from=${ISO_STORAGE}:import/${TEMPLATE_NAME}.qcow2,discard=on,iothread=1" \
    --boot order=scsi0 --serial0 socket --vga serial0 --agent enabled=1
  qm template "${TEMPLATE_VMID}"
}

usage() {
  cat >&2 <<EOF
usage: $0 <step>...
  role       create/re-sync the ${ROLE} role
  token      create ${PVE_USER} and its privilege-separated API token; grant ACLs
  cert       issue a pveproxy certificate carrying every node address as a SAN
  template   import ${TEMPLATE_NAME} as template VMID ${TEMPLATE_VMID}
  all        role token cert template
EOF
  exit 2
}

main() {
  [ $# -gt 0 ] || usage
  require_pve
  for s in "$@"; do
    case "${s}" in
      role) step_role ;;
      token) step_role; step_token ;;
      cert) step_cert ;;
      template) step_template ;;
      all) step_role; step_token; step_cert; step_template ;;
      *) usage ;;
    esac
  done
}

main "$@"
