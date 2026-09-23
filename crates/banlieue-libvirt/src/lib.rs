// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! # banlieue-libvirt
//!
//! A minimal client for libvirt's remote RPC protocol, implementing only the
//! subset banlieue needs.
//!
//! ## Why this exists rather than a dependency
//!
//! The available options were all unsuitable (ADR-0011): `virt`/`virt-sys` are
//! FFI bindings to the libvirt C library — a native dependency in an otherwise
//! distroless, cross-compiled image — while `libvirt` (2015) and `libvirt-rpc`
//! (2018) are abandoned. Measured against the protocol's actual size, a
//! first-party client is a few hundred lines and needs **no new
//! dependencies**: the codec and framing are here, and the transport reuses
//! `rustls`, already pinned in the workspace.
//!
//! ## Scope
//!
//! Deliberately small. This crate speaks enough of the protocol to register a
//! libvirt host as a `Provider` and import a disk image into a storage pool.
//! It is not a general-purpose libvirt binding and does not aim to become one;
//! domain/VM lifecycle is out of scope until the `LibvirtMachine` work.
//!
//! It also carries no `kube` dependency and no banlieue API types, so the
//! image-import Job can link it without pulling in the controller's dependency
//! graph.

pub mod procs;
pub mod rpc;
pub mod transport;
pub mod xdr;

pub use procs::{
    AGENT_TIMEOUT_DEFAULT, AuthType, CONNECT_RO, DEVICE_MODIFY_CONFIG, DEVICE_MODIFY_EJECT,
    DEVICE_MODIFY_FORCE, DEVICE_MODIFY_LIVE, DOMAIN_INTERFACE_MAX, DOMAIN_IP_ADDR_MAX,
    DOMAIN_UNDEFINE_EPHEMERAL, DOMAIN_UNDEFINE_MANAGED_SAVE, DOMAIN_UNDEFINE_NVRAM,
    DOMAIN_UNDEFINE_TPM, DhcpLease, Domain, DomainInterface, DomainIpAddr, DomainState,
    InterfaceAddressSource, NETWORK_DHCP_LEASES_MAX, Network, QEMU_PROC_DOMAIN_AGENT_COMMAND,
    StoragePool, StorageVol, UUID_LEN, VIR_ERR_NO_DOMAIN, VIR_ERR_NO_STORAGE_POOL,
    VIR_ERR_NO_STORAGE_VOL, VOLUME_NAME_MAX, auth_list, connect_open,
    decode_domain_qemu_agent_command_ret, decode_network_get_dhcp_leases_ret, domain_create,
    domain_define_xml, domain_destroy, domain_get_state, domain_interface_addresses,
    domain_lookup_by_name, domain_qemu_agent_command, domain_shutdown, domain_undefine,
    domain_update_device_flags, encode_domain_qemu_agent_command_args,
    encode_domain_update_device_flags_args, encode_network_get_dhcp_leases_args, is_not_found,
    list_all_networks, list_all_storage_pools, network_get_dhcp_leases, qcow2_overlay_volume_xml,
    qcow2_volume_xml, raw_volume_xml, storage_pool_list_all_volumes, storage_pool_lookup_by_name,
    storage_pool_refresh, storage_vol_create_xml, storage_vol_delete, storage_vol_lookup_by_name,
    storage_vol_upload, validate_volume_name,
};

pub use rpc::{
    MESSAGE_HEADER_LEN, MESSAGE_LEN_PREFIX_LEN, MESSAGE_MAX, MessageHeader, MessageStatus,
    MessageType, PAYLOAD_MAX, PROC_CONNECT_CLOSE, PROC_CONNECT_LIST_ALL_NETWORKS,
    PROC_CONNECT_LIST_ALL_STORAGE_POOLS, PROC_CONNECT_OPEN, PROC_STORAGE_POOL_GET_XML_DESC,
    PROC_STORAGE_POOL_LIST_ALL_VOLUMES, PROC_STORAGE_POOL_LOOKUP_BY_NAME,
    PROC_STORAGE_POOL_REFRESH, PROC_STORAGE_VOL_CREATE_XML, PROC_STORAGE_VOL_UPLOAD, QEMU_PROGRAM,
    QEMU_PROTOCOL_VERSION, REMOTE_PROGRAM, REMOTE_PROTOCOL_VERSION, RpcError, decode_message,
    encode_message, parse_length_prefix,
};
pub use transport::{
    DEFAULT_TIMEOUT, DEFAULT_TLS_PORT, Result, Session, TlsIdentity, TransportError, connect_tls,
    connect_tls_with_timeout,
};
pub use xdr::{Decoder, Encoder, XdrError};
