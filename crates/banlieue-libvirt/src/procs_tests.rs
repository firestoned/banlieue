// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `procs.rs`.
//!
//! These pin the argument encodings byte-for-byte and decode hand-built reply
//! payloads. That validates our *reading* of `remote_protocol.x` — it cannot
//! validate that the reading is correct. Only a round-trip against a live
//! libvirtd can do that, which is why ADR-0011 treats the integration test as
//! non-optional.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::xdr::Encoder;

    fn uuid_bytes(seed: u8) -> [u8; UUID_LEN] {
        let mut u = [0u8; UUID_LEN];
        for (i, b) in u.iter_mut().enumerate() {
            *b = seed.wrapping_add(i as u8);
        }
        u
    }

    /// Build a `pools<>` / `nets<>` reply payload: count, then
    /// `{name, uuid}` elements, then the trailing `unsigned int ret`.
    fn list_payload(entries: &[(&str, [u8; UUID_LEN])]) -> Vec<u8> {
        let mut e = Encoder::new();
        e.write_u32(entries.len() as u32);
        for (name, uuid) in entries {
            e.write_string(name);
            e.write_opaque_fixed(uuid);
        }
        e.write_u32(entries.len() as u32); // trailing total
        e.into_bytes()
    }

    // ------------------------------------------------------------------
    // Argument encoding
    // ------------------------------------------------------------------

    #[test]
    fn connect_open_encodes_uri_as_an_xdr_optional() {
        // remote_string is a POINTER: a bool, then the string only when set.
        // Writing the string unconditionally would shift `flags`.
        let args = encode_connect_open_args(Some("qemu:///system"), 0);
        let mut expected = Encoder::new();
        expected.write_bool(true);
        expected.write_string("qemu:///system");
        expected.write_u32(0);
        assert_eq!(args, expected.into_bytes());
    }

    #[test]
    fn connect_open_with_no_uri_writes_only_the_false_discriminant() {
        let args = encode_connect_open_args(None, 0);
        // bool(false) + flags, and crucially NO string in between.
        assert_eq!(args, vec![0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn connect_open_read_only_sets_vir_connect_ro() {
        let args = encode_connect_open_args(None, CONNECT_RO);
        assert_eq!(args[4..8], [0, 0, 0, 1]);
        assert_eq!(CONNECT_RO, 1);
    }

    #[test]
    fn list_all_args_encode_need_results_then_flags() {
        assert_eq!(encode_list_all_args(true, 0), vec![0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(encode_list_all_args(false, 0), vec![0, 0, 0, 0, 0, 0, 0, 0]);
    }

    // ------------------------------------------------------------------
    // Reply decoding
    // ------------------------------------------------------------------

    #[test]
    fn decode_storage_pools_reads_name_and_raw_uuid() {
        let payload = list_payload(&[("default", uuid_bytes(1)), ("k0s-bootstrap", uuid_bytes(9))]);
        let pools = decode_storage_pools(&payload).unwrap();
        assert_eq!(
            pools,
            vec![
                StoragePool {
                    name: "default".into(),
                    uuid: uuid_bytes(1)
                },
                StoragePool {
                    name: "k0s-bootstrap".into(),
                    uuid: uuid_bytes(9)
                },
            ]
        );
    }

    #[test]
    fn uuid_is_sixteen_raw_bytes_not_a_string() {
        // VIR_UUID_BUFLEN is 16; the 36-char string form never appears on the
        // wire. It is fixed-length opaque, so it carries no length prefix.
        assert_eq!(UUID_LEN, 16);
        let payload = list_payload(&[("p", uuid_bytes(0))]);
        // 4 (count) + 4+4 (string "p" padded) + 16 (uuid) + 4 (trailing ret)
        assert_eq!(payload.len(), 4 + 8 + 16 + 4);
        assert_eq!(decode_storage_pools(&payload).unwrap()[0].uuid.len(), 16);
    }

    #[test]
    fn decode_networks_reads_the_same_shape() {
        let payload = list_payload(&[("default", uuid_bytes(3))]);
        let nets = decode_networks(&payload).unwrap();
        assert_eq!(nets[0].name, "default");
        assert_eq!(nets[0].uuid, uuid_bytes(3));
    }

    #[test]
    fn empty_list_decodes_to_an_empty_vec() {
        let payload = list_payload(&[]);
        assert!(decode_storage_pools(&payload).unwrap().is_empty());
        assert!(decode_networks(&payload).unwrap().is_empty());
    }

    #[test]
    fn a_count_beyond_the_protocol_maximum_is_rejected_before_allocating() {
        // The count comes off the network. Trusting it would let four bytes
        // request a multi-gigabyte Vec.
        let mut e = Encoder::new();
        e.write_u32(u32::MAX);
        let err = decode_storage_pools(&e.into_bytes()).unwrap_err();
        assert!(
            matches!(err, TransportError::Protocol { .. }),
            "expected Protocol, got {err:?}"
        );
    }

    #[test]
    fn a_count_larger_than_the_payload_fails_cleanly() {
        // Claims 5 entries but supplies none: must surface as a decode error,
        // not a panic or a partially-filled Vec.
        let mut e = Encoder::new();
        e.write_u32(5);
        assert!(decode_storage_pools(&e.into_bytes()).is_err());
    }

    #[test]
    fn a_truncated_uuid_fails_cleanly() {
        let mut e = Encoder::new();
        e.write_u32(1);
        e.write_string("p");
        e.write_opaque_fixed(&[0u8; 8]); // half a UUID
        assert!(decode_storage_pools(&e.into_bytes()).is_err());
    }

    // ------------------------------------------------------------------
    // End-to-end over a scripted peer
    // ------------------------------------------------------------------

    /// Drive an exchange over a scripted peer that answers `AUTH_LIST` with
    /// `REMOTE_AUTH_NONE`, then returns the procedure and argument bytes of
    /// the LAST call it saw — the one actually under test.
    ///
    /// `connect_open` performs two calls now (auth negotiation, then open),
    /// so a single-call harness would deadlock.
    async fn capture_last_call_args<F, Fut>(exercise: F) -> (i32, Vec<u8>)
    where
        F: FnOnce(crate::transport::Session<tokio::io::DuplexStream>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        use crate::rpc::{
            MessageHeader, MessageStatus, MessageType, decode_message, encode_message,
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (client, mut peer) = tokio::io::duplex(8192);
        let server = tokio::spawn(async move {
            let mut last = (0i32, Vec::new());
            loop {
                let mut prefix = [0u8; 4];
                if peer.read_exact(&mut prefix).await.is_err() {
                    break; // client finished and dropped the stream
                }
                let total = u32::from_be_bytes(prefix) as usize;
                let mut rest = vec![0u8; total - 4];
                peer.read_exact(&mut rest).await.unwrap();
                let mut framed = prefix.to_vec();
                framed.extend_from_slice(&rest);
                let (h, args) = decode_message(&framed).unwrap();
                last = (h.procedure, args.to_vec());

                // AUTH_LIST must be answered with a types<> array offering NONE,
                // or connect_open refuses to proceed.
                let payload = if h.procedure == crate::rpc::PROC_AUTH_LIST {
                    let mut e = Encoder::new();
                    e.write_u32(1);
                    e.write_i32(0); // REMOTE_AUTH_NONE
                    e.into_bytes()
                } else {
                    Vec::new()
                };
                let reply = encode_message(
                    &MessageHeader {
                        message_type: MessageType::Reply,
                        status: MessageStatus::Ok,
                        ..h
                    },
                    &payload,
                );
                peer.write_all(&reply).await.unwrap();
            }
            last
        });

        exercise(crate::transport::Session::new(client)).await;
        server.await.unwrap()
    }

    #[tokio::test]
    async fn connect_open_plumbs_read_only_through_to_the_wire() {
        // Encoding CONNECT_RO correctly is not enough — `connect_open` must
        // actually pass it. Opening read-write when the caller asked for
        // read-only is a silent least-privilege failure, so assert on the
        // bytes that reach the peer rather than on the encoder in isolation.
        let (proc, args) = capture_last_call_args(|mut s| async move {
            connect_open(&mut s, Some("qemu:///system"), true)
                .await
                .unwrap();
        })
        .await;
        assert_eq!(proc, crate::rpc::PROC_CONNECT_OPEN);
        assert_eq!(
            args,
            encode_connect_open_args(Some("qemu:///system"), CONNECT_RO)
        );

        let (_, args) = capture_last_call_args(|mut s| async move {
            connect_open(&mut s, Some("qemu:///system"), false)
                .await
                .unwrap();
        })
        .await;
        assert_eq!(args, encode_connect_open_args(Some("qemu:///system"), 0));
    }

    #[tokio::test]
    async fn list_all_storage_pools_round_trips_over_a_session() {
        use crate::rpc::{
            MessageHeader, MessageStatus, MessageType, decode_message, encode_message,
        };
        use crate::transport::Session;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (client, mut peer) = tokio::io::duplex(8192);
        let mut session = Session::new(client);

        let server = tokio::spawn(async move {
            let mut prefix = [0u8; 4];
            peer.read_exact(&mut prefix).await.unwrap();
            let total = u32::from_be_bytes(prefix) as usize;
            let mut rest = vec![0u8; total - 4];
            peer.read_exact(&mut rest).await.unwrap();
            let mut framed = prefix.to_vec();
            framed.extend_from_slice(&rest);
            let (h, args) = decode_message(&framed).unwrap();

            // The call must target the right procedure with need_results set.
            assert_eq!(h.procedure, crate::rpc::PROC_CONNECT_LIST_ALL_STORAGE_POOLS);
            assert_eq!(args, encode_list_all_args(true, 0));

            let reply = encode_message(
                &MessageHeader {
                    message_type: MessageType::Reply,
                    status: MessageStatus::Ok,
                    ..h
                },
                &list_payload(&[("default", uuid_bytes(2))]),
            );
            peer.write_all(&reply).await.unwrap();
        });

        let pools = list_all_storage_pools(&mut session).await.unwrap();
        assert_eq!(pools.len(), 1);
        assert_eq!(pools[0].name, "default");
        server.await.unwrap();
    }

    // ------------------------------------------------------------------
    // Volume create + streaming upload
    // ------------------------------------------------------------------

    #[test]
    fn storage_vol_has_three_strings_and_no_uuid() {
        // remote_nonnull_storage_vol is {pool, name, key} — unlike pools and
        // networks it carries NO uuid. Assuming symmetry here would shift
        // every following field.
        let v = StorageVol {
            pool: "default".into(),
            name: "img.raw".into(),
            key: "/var/lib/libvirt/images/img.raw".into(),
        };
        let mut e = Encoder::new();
        e.write_string(&v.pool);
        e.write_string(&v.name);
        e.write_string(&v.key);
        let encoded = e.into_bytes();
        let mut d = crate::xdr::Decoder::new(&encoded);
        assert_eq!(d.read_string().unwrap(), "default");
        assert_eq!(d.read_string().unwrap(), "img.raw");
        assert_eq!(d.read_string().unwrap(), "/var/lib/libvirt/images/img.raw");
        assert_eq!(d.remaining(), 0, "no uuid field follows");
    }

    #[test]
    fn raw_volume_xml_declares_raw_format_and_byte_capacity() {
        let xml = raw_volume_xml("disk.raw", 1_048_576);
        assert!(xml.contains("<name>disk.raw</name>"));
        assert!(xml.contains("<capacity unit='bytes'>1048576</capacity>"));
        assert!(xml.contains("<format type='raw'/>"));
        // qcow2 would need qemu-img, which ADR-0011 removes from the pipeline.
        assert!(!xml.contains("qcow2"));
    }

    #[test]
    fn stream_chunk_max_matches_the_observed_wire_size() {
        // A real virsh vol-upload sent full packets of len=262148 on the wire:
        // 4 (length prefix) + 24 (header) + 262120 (payload).
        assert_eq!(crate::rpc::STREAM_CHUNK_MAX, 262_120);
        assert_eq!(
            crate::rpc::MESSAGE_LEN_PREFIX_LEN
                + crate::rpc::MESSAGE_HEADER_LEN
                + crate::rpc::STREAM_CHUNK_MAX,
            262_148
        );
    }

    #[tokio::test]
    async fn upload_chunks_data_and_terminates_the_stream() {
        use crate::rpc::{
            MessageHeader, MessageStatus, MessageType, decode_message, encode_message,
        };
        use crate::transport::Session;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Two full chunks plus a partial, to exercise the boundary.
        let total = crate::rpc::STREAM_CHUNK_MAX * 2 + 100;
        let source: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
        let expected = source.clone();

        let (client, mut peer) = tokio::io::duplex(1 << 20);
        let mut session = Session::new(client);

        let server = tokio::spawn(async move {
            let mut received = Vec::new();
            let mut kinds = Vec::new();
            loop {
                let mut prefix = [0u8; 4];
                if peer.read_exact(&mut prefix).await.is_err() {
                    break;
                }
                let total_len = u32::from_be_bytes(prefix) as usize;
                let mut rest = vec![0u8; total_len - 4];
                peer.read_exact(&mut rest).await.unwrap();
                let mut framed = prefix.to_vec();
                framed.extend_from_slice(&rest);
                let (h, body) = decode_message(&framed).unwrap();
                kinds.push((h.message_type, h.status, h.serial));

                match (h.message_type, h.status) {
                    // The opening CALL: reply so the client may start sending.
                    (MessageType::Call, _) => {
                        peer.write_all(&encode_message(
                            &MessageHeader {
                                message_type: MessageType::Reply,
                                status: MessageStatus::Ok,
                                ..h
                            },
                            &[],
                        ))
                        .await
                        .unwrap();
                    }
                    // Data packets carry raw, un-encoded bytes.
                    (MessageType::Stream, MessageStatus::Continue) => {
                        received.extend_from_slice(body)
                    }
                    // EOF: confirm, mirroring a real libvirtd.
                    (MessageType::Stream, MessageStatus::Ok) => {
                        peer.write_all(&encode_message(&h, &[])).await.unwrap();
                        break;
                    }
                    _ => panic!("unexpected {:?}/{:?}", h.message_type, h.status),
                }
            }
            (received, kinds)
        });

        let mut reader = std::io::Cursor::new(source);
        storage_vol_upload(
            &mut session,
            &StorageVol {
                pool: "default".into(),
                name: "disk.raw".into(),
                key: "/k".into(),
            },
            &mut reader,
            total as u64,
        )
        .await
        .unwrap();

        let (received, kinds) = server.await.unwrap();
        assert_eq!(received, expected, "uploaded bytes must match the source");

        // Shape: one Call, three Continue packets, one Ok terminator.
        let serial = kinds[0].2;
        assert_eq!(kinds[0].0, MessageType::Call);
        let continues = kinds
            .iter()
            .filter(|(t, s, _)| *t == MessageType::Stream && *s == MessageStatus::Continue)
            .count();
        assert_eq!(continues, 3, "2 full chunks + 1 partial");
        assert!(
            kinds.iter().all(|(_, _, s)| *s == serial),
            "every stream packet reuses the call's serial"
        );
        assert_eq!(
            kinds.last().unwrap().1,
            MessageStatus::Ok,
            "stream must be terminated with Ok"
        );
    }

    #[tokio::test]
    async fn upload_refuses_a_source_shorter_than_declared() {
        // Silently uploading a truncated image would corrupt the volume.
        use crate::rpc::{
            MessageHeader, MessageStatus, MessageType, decode_message, encode_message,
        };
        use crate::transport::Session;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (client, mut peer) = tokio::io::duplex(65536);
        let mut session = Session::new(client);
        tokio::spawn(async move {
            let mut prefix = [0u8; 4];
            peer.read_exact(&mut prefix).await.unwrap();
            let n = u32::from_be_bytes(prefix) as usize;
            let mut rest = vec![0u8; n - 4];
            peer.read_exact(&mut rest).await.unwrap();
            let mut framed = prefix.to_vec();
            framed.extend_from_slice(&rest);
            let (h, _) = decode_message(&framed).unwrap();
            peer.write_all(&encode_message(
                &MessageHeader {
                    message_type: MessageType::Reply,
                    status: MessageStatus::Ok,
                    ..h
                },
                &[],
            ))
            .await
            .unwrap();
            // Drain whatever the client sends next.
            let mut sink = vec![0u8; 65536];
            let _ = peer.read(&mut sink).await;
        });

        let mut reader = std::io::Cursor::new(vec![0u8; 10]);
        let err = storage_vol_upload(
            &mut session,
            &StorageVol {
                pool: "p".into(),
                name: "n".into(),
                key: "k".into(),
            },
            &mut reader,
            100, // claims 100 bytes, supplies 10
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, TransportError::Protocol { .. }),
            "expected Protocol, got {err:?}"
        );
    }

    // ------------------------------------------------------------------
    // Domain handle (`remote_nonnull_domain`) — ADR-0050
    // ------------------------------------------------------------------

    /// `remote_nonnull_domain { string name; uuid[16]; int id; }`. The `id`
    /// is a *trailing* field: an encoder that stops after the UUID (as the
    /// storage-pool handle legitimately does) shifts every following
    /// argument, which is the failure this pins.
    #[test]
    fn domain_handle_round_trips() {
        let dom = Domain {
            name: "sandbox-01".into(),
            uuid: uuid_bytes(0x40),
            id: 7,
        };
        let mut e = Encoder::new();
        dom.encode(&mut e);
        let bytes = e.into_bytes();

        // 4 len + 10 name + 2 pad + 16 uuid + 4 id
        assert_eq!(bytes.len(), 4 + 12 + UUID_LEN + 4);
        assert_eq!(&bytes[bytes.len() - 4..], &7i32.to_be_bytes());

        let mut d = Decoder::new(&bytes);
        assert_eq!(Domain::decode(&mut d).unwrap(), dom);
        assert!(d.is_empty(), "decode must consume the whole handle");
    }

    /// An inactive domain is `id == -1` on the wire, not `0`.
    #[test]
    fn domain_handle_accepts_inactive_id() {
        let dom = Domain {
            name: "off".into(),
            uuid: uuid_bytes(1),
            id: -1,
        };
        let mut e = Encoder::new();
        dom.encode(&mut e);
        let mut d = Decoder::new(e.as_bytes());
        assert_eq!(Domain::decode(&mut d).unwrap().id, -1);
    }

    // ------------------------------------------------------------------
    // Domain argument encoding
    // ------------------------------------------------------------------

    #[test]
    fn encodes_domain_lookup_by_name_args() {
        let mut want = Encoder::new();
        want.write_string("sandbox-01");
        assert_eq!(
            encode_domain_lookup_by_name_args("sandbox-01"),
            want.into_bytes()
        );
    }

    #[test]
    fn encodes_domain_define_xml_flags_args() {
        let mut want = Encoder::new();
        want.write_string("<domain/>");
        want.write_u32(0);
        assert_eq!(
            encode_domain_define_xml_flags_args("<domain/>", 0),
            want.into_bytes()
        );
    }

    #[test]
    fn encodes_domain_only_args() {
        let dom = Domain {
            name: "d".into(),
            uuid: uuid_bytes(2),
            id: 3,
        };
        let mut want = Encoder::new();
        dom.encode(&mut want);
        assert_eq!(encode_domain_args(&dom), want.into_bytes());
    }

    #[test]
    fn encodes_domain_flags_args() {
        let dom = Domain {
            name: "d".into(),
            uuid: uuid_bytes(3),
            id: 4,
        };
        let mut want = Encoder::new();
        dom.encode(&mut want);
        want.write_u32(DOMAIN_UNDEFINE_EPHEMERAL);
        assert_eq!(
            encode_domain_flags_args(&dom, DOMAIN_UNDEFINE_EPHEMERAL),
            want.into_bytes()
        );
    }

    #[test]
    fn encodes_domain_interface_addresses_args() {
        let dom = Domain {
            name: "d".into(),
            uuid: uuid_bytes(4),
            id: 5,
        };
        let mut want = Encoder::new();
        dom.encode(&mut want);
        want.write_u32(InterfaceAddressSource::Agent as u32);
        want.write_u32(0);
        assert_eq!(
            encode_domain_interface_addresses_args(&dom, InterfaceAddressSource::Agent, 0),
            want.into_bytes()
        );
    }

    // ------------------------------------------------------------------
    // Undefine flags — the teardown-leak guard
    // ------------------------------------------------------------------

    /// Transcribed from `libvirt-domain.h`: MANAGED_SAVE 1<<0, NVRAM 1<<2,
    /// TPM 1<<5. A previous live incident had `virsh undefine` silently fail
    /// on UEFI domains for want of `--nvram`, leaving every VM defined while
    /// teardown reported success. On libvirt these three flags are what stop
    /// a deleted single-use sandbox from leaving its varstore and swtpm state
    /// behind (ADR-0050 Decision 5).
    #[test]
    fn undefine_flag_values_match_libvirt() {
        assert_eq!(DOMAIN_UNDEFINE_MANAGED_SAVE, 1);
        assert_eq!(DOMAIN_UNDEFINE_NVRAM, 4);
        assert_eq!(DOMAIN_UNDEFINE_TPM, 32);
        assert_eq!(DOMAIN_UNDEFINE_EPHEMERAL, 1 | 4 | 32);
        assert_eq!(DOMAIN_UNDEFINE_EPHEMERAL, 0x25);
    }

    /// `KEEP_NVRAM` (1<<3) and `KEEP_TPM` (1<<6) are the opposites of what a
    /// single-use VM wants; this pins that neither ever creeps into the
    /// composite.
    #[test]
    fn undefine_ephemeral_keeps_nothing() {
        const KEEP_NVRAM: u32 = 1 << 3;
        const KEEP_TPM: u32 = 1 << 6;
        assert_eq!(DOMAIN_UNDEFINE_EPHEMERAL & KEEP_NVRAM, 0);
        assert_eq!(DOMAIN_UNDEFINE_EPHEMERAL & KEEP_TPM, 0);
    }

    // ------------------------------------------------------------------
    // Domain reply decoding
    // ------------------------------------------------------------------

    #[test]
    fn decodes_domain_ret() {
        let dom = Domain {
            name: "sandbox-02".into(),
            uuid: uuid_bytes(9),
            id: 11,
        };
        let mut e = Encoder::new();
        dom.encode(&mut e);
        assert_eq!(decode_domain_ret(e.as_bytes()).unwrap(), dom);
    }

    #[test]
    fn decodes_domain_state() {
        let mut e = Encoder::new();
        e.write_i32(DOMAIN_STATE_RUNNING);
        e.write_i32(0); // reason
        assert_eq!(
            decode_domain_get_state_ret(e.as_bytes()).unwrap(),
            DomainState::Running
        );
    }

    /// Every state libvirt defines maps to a named variant; an unknown one is
    /// carried through rather than collapsed onto a guess, so a future
    /// libvirt state shows up in logs instead of being silently read as
    /// "running".
    #[test]
    fn decodes_every_known_domain_state() {
        let cases = [
            (0, DomainState::NoState),
            (1, DomainState::Running),
            (2, DomainState::Blocked),
            (3, DomainState::Paused),
            (4, DomainState::ShuttingDown),
            (5, DomainState::ShutOff),
            (6, DomainState::Crashed),
            (7, DomainState::PmSuspended),
            (99, DomainState::Unknown(99)),
        ];
        for (wire, want) in cases {
            let mut e = Encoder::new();
            e.write_i32(wire);
            e.write_i32(0);
            assert_eq!(
                decode_domain_get_state_ret(e.as_bytes()).unwrap(),
                want,
                "state {wire}"
            );
        }
    }

    /// Only `ShutOff` and `Crashed` mean "this domain is not executing".
    /// `ShuttingDown` is in-progress and must not be read as stopped, or a
    /// reconciler will undefine a domain that is still running.
    #[test]
    fn domain_state_is_running_is_conservative() {
        assert!(DomainState::Running.is_running());
        assert!(DomainState::Blocked.is_running());
        assert!(DomainState::Paused.is_running());
        assert!(DomainState::ShuttingDown.is_running());
        assert!(DomainState::PmSuspended.is_running());
        assert!(!DomainState::ShutOff.is_running());
        assert!(!DomainState::Crashed.is_running());
        assert!(!DomainState::NoState.is_running());
        assert!(!DomainState::Unknown(42).is_running());
    }

    /// One address in a test fixture: `(type, addr, prefix)`.
    type FixtureAddr<'a> = (i32, &'a str, u32);
    /// One interface in a test fixture: `(name, hwaddr, addrs)`.
    type FixtureIface<'a> = (&'a str, Option<&'a str>, &'a [FixtureAddr<'a>]);

    /// Build a `remote_domain_interface_addresses_ret` payload.
    fn iface_payload(ifaces: &[FixtureIface<'_>]) -> Vec<u8> {
        let mut e = Encoder::new();
        e.write_u32(ifaces.len() as u32);
        for (name, hwaddr, addrs) in ifaces {
            e.write_string(name);
            // remote_string hwaddr — a pointer type, so an optional.
            match hwaddr {
                Some(h) => {
                    e.write_bool(true);
                    e.write_string(h);
                }
                None => e.write_bool(false),
            }
            e.write_u32(addrs.len() as u32);
            for (kind, addr, prefix) in *addrs {
                e.write_i32(*kind);
                e.write_string(addr);
                e.write_u32(*prefix);
            }
        }
        e.into_bytes()
    }

    #[test]
    fn decodes_interface_addresses() {
        let body = iface_payload(&[
            ("lo", Some("00:00:00:00:00:00"), &[(0, "127.0.0.1", 8)]),
            (
                "enp1s0",
                Some("52:54:00:aa:bb:cc"),
                &[(0, "192.0.2.24", 24), (1, "2001:db8::24", 64)],
            ),
        ]);
        let ifaces = decode_domain_interface_addresses_ret(&body).unwrap();

        assert_eq!(ifaces.len(), 2);
        assert_eq!(ifaces[1].name, "enp1s0");
        assert_eq!(ifaces[1].hwaddr.as_deref(), Some("52:54:00:aa:bb:cc"));
        assert_eq!(ifaces[1].addrs.len(), 2);
        assert_eq!(ifaces[1].addrs[0].addr, "192.0.2.24");
        assert_eq!(ifaces[1].addrs[0].prefix, 24);
        assert_eq!(ifaces[1].addrs[1].addr, "2001:db8::24");
    }

    /// `hwaddr` is `remote_string` (a pointer), so it is an optional on the
    /// wire. Reading it unconditionally shifts every following field — the
    /// same trap `procs.rs`'s module doc calls out for `connect_open`.
    #[test]
    fn decodes_interface_with_absent_hwaddr() {
        let body = iface_payload(&[("dummy", None, &[(0, "192.0.2.9", 24)])]);
        let ifaces = decode_domain_interface_addresses_ret(&body).unwrap();
        assert_eq!(ifaces.len(), 1);
        assert!(ifaces[0].hwaddr.is_none());
        assert_eq!(ifaces[0].addrs[0].addr, "192.0.2.9");
    }

    #[test]
    fn decodes_interface_with_no_addresses() {
        let body = iface_payload(&[("enp1s0", Some("52:54:00:00:00:01"), &[])]);
        let ifaces = decode_domain_interface_addresses_ret(&body).unwrap();
        assert!(ifaces[0].addrs.is_empty());
    }

    /// A hostile or corrupt interface count must be rejected against
    /// `REMOTE_DOMAIN_INTERFACE_MAX` before it reserves capacity, the same
    /// guard the pool and network lists already apply.
    #[test]
    fn rejects_oversized_interface_count() {
        let mut e = Encoder::new();
        e.write_u32(DOMAIN_INTERFACE_MAX as u32 + 1);
        let err = decode_domain_interface_addresses_ret(e.as_bytes()).unwrap_err();
        assert!(
            matches!(err, TransportError::Protocol { .. }),
            "expected Protocol, got {err:?}"
        );
    }

    /// Likewise for the per-interface address count against
    /// `REMOTE_DOMAIN_IP_ADDR_MAX`.
    #[test]
    fn rejects_oversized_address_count() {
        let mut e = Encoder::new();
        e.write_u32(1);
        e.write_string("enp1s0");
        e.write_bool(false);
        e.write_u32(DOMAIN_IP_ADDR_MAX as u32 + 1);
        let err = decode_domain_interface_addresses_ret(e.as_bytes()).unwrap_err();
        assert!(
            matches!(err, TransportError::Protocol { .. }),
            "expected Protocol, got {err:?}"
        );
    }

    // ------------------------------------------------------------------
    // Volume names — a guard, not an escaper
    // ------------------------------------------------------------------

    /// Volume names reach libvirt inside XML. banlieue always generates them
    /// from Kubernetes object names, which are already DNS-1123-restricted,
    /// so the realistic risk is not injection but a hand-written
    /// `LibvirtMachine` carrying something odd. Rejecting is better than
    /// escaping here: a mangled-but-accepted volume name creates a file
    /// nobody can find again, while an error is visible immediately.
    #[test]
    fn accepts_the_volume_names_banlieue_generates() {
        for ok in [
            "sandbox-01-os.qcow2",
            "banlieue-system-db-01-cidata.iso",
            "a",
            "ubuntu_22.04-base.img",
        ] {
            assert!(validate_volume_name(ok).is_ok(), "{ok} should be accepted");
        }
    }

    #[test]
    fn rejects_volume_names_that_could_reach_xml_or_the_filesystem() {
        for bad in [
            "",                    // empty
            "a/b.qcow2",           // path separator — escapes the pool
            "../escape.qcow2",     // traversal
            "disk'/><x a='.qcow2", // markup
            "disk name.qcow2",     // space
            "disk\u{0}.qcow2",     // NUL
        ] {
            assert!(
                validate_volume_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_an_over_long_volume_name() {
        let long = "a".repeat(VOLUME_NAME_MAX + 1);
        assert!(validate_volume_name(&long).is_err());
        let at_limit = "a".repeat(VOLUME_NAME_MAX);
        assert!(validate_volume_name(&at_limit).is_ok());
    }

    // ------------------------------------------------------------------
    // qcow2 volume XML
    // ------------------------------------------------------------------

    /// The `Deferred` shape: an empty disk the guest installs itself onto.
    #[test]
    fn empty_qcow2_volume_declares_size_and_no_backing_store() {
        let xml = qcow2_volume_xml("sandbox-01-os.qcow2", 42 * 1024 * 1024 * 1024).unwrap();
        assert!(xml.contains("<name>sandbox-01-os.qcow2</name>"), "{xml}");
        assert!(xml.contains("<format type='qcow2'/>"), "{xml}");
        assert!(xml.contains("45097156608"), "{xml}");
        assert!(!xml.contains("backingStore"), "{xml}");
    }

    /// The `Immediate` shape: a copy-on-write overlay. The backing store's
    /// own format must be declared — libvirt does not probe it, and an
    /// undeclared backing format is both a security advisory (CVE-2010-2238
    /// class) and, on modern libvirt, a hard refusal.
    #[test]
    fn overlay_qcow2_volume_declares_its_backing_store_and_format() {
        let xml = qcow2_overlay_volume_xml(
            "sandbox-01-os.qcow2",
            42 * 1024 * 1024 * 1024,
            "/var/lib/libvirt/images/base.qcow2",
            "qcow2",
        )
        .unwrap();
        assert!(xml.contains("<backingStore>"), "{xml}");
        assert!(
            xml.contains("<path>/var/lib/libvirt/images/base.qcow2</path>"),
            "{xml}"
        );
        assert!(xml.contains("<format type='qcow2'/>"), "{xml}");
        // Both the volume's own format and the backing store's.
        assert_eq!(xml.matches("<format type=").count(), 2, "{xml}");
    }

    /// A raw backing image (what `banlieue-imagebuilder` uploads, ADR-0011)
    /// with a qcow2 overlay on top is the normal combination, so the two
    /// formats genuinely differ and must not be conflated.
    #[test]
    fn overlay_can_sit_on_a_raw_backing_image() {
        let xml = qcow2_overlay_volume_xml("o.qcow2", 1024, "/img/base.img", "raw").unwrap();
        assert!(xml.contains("<format type='raw'/>"), "{xml}");
        assert!(xml.contains("<format type='qcow2'/>"), "{xml}");
    }

    #[test]
    fn qcow2_builders_reject_a_bad_volume_name() {
        assert!(qcow2_volume_xml("a/b", 1024).is_err());
        assert!(qcow2_overlay_volume_xml("a/b", 1024, "/img/x", "raw").is_err());
    }

    /// A backing path containing markup would otherwise close the element.
    /// Paths are not names, so they cannot use the same allowlist — they are
    /// rejected on the characters XML and shell-free path handling cannot
    /// carry.
    #[test]
    fn overlay_rejects_a_hostile_backing_path() {
        let err = qcow2_overlay_volume_xml("o.qcow2", 1024, "/img/x'/><x a='", "raw");
        assert!(err.is_err());
    }

    // ------------------------------------------------------------------
    // Storage lookup / delete / refresh
    // ------------------------------------------------------------------

    fn sample_pool() -> StoragePool {
        StoragePool {
            name: "default".into(),
            uuid: uuid_bytes(0x20),
        }
    }

    #[test]
    fn encodes_storage_pool_lookup_by_name_args() {
        let mut want = Encoder::new();
        want.write_string("default");
        assert_eq!(
            encode_storage_pool_lookup_by_name_args("default"),
            want.into_bytes()
        );
    }

    /// `remote_nonnull_storage_pool` is name + uuid and, unlike a domain
    /// handle, has **no trailing id**. Adding one would shift the `name`
    /// that follows it in the vol-lookup args.
    #[test]
    fn encodes_storage_vol_lookup_by_name_args() {
        let pool = sample_pool();
        let mut want = Encoder::new();
        want.write_string(&pool.name);
        want.write_opaque_fixed(&pool.uuid);
        want.write_string("disk.qcow2");
        assert_eq!(
            encode_storage_vol_lookup_by_name_args(&pool, "disk.qcow2"),
            want.into_bytes()
        );
    }

    #[test]
    fn encodes_storage_vol_delete_args() {
        let vol = StorageVol {
            pool: "default".into(),
            name: "disk.qcow2".into(),
            key: "/var/lib/libvirt/images/disk.qcow2".into(),
        };
        let mut want = Encoder::new();
        want.write_string(&vol.pool);
        want.write_string(&vol.name);
        want.write_string(&vol.key);
        want.write_u32(0);
        assert_eq!(encode_storage_vol_delete_args(&vol), want.into_bytes());
    }

    #[test]
    fn encodes_storage_pool_refresh_args() {
        let pool = sample_pool();
        let mut want = Encoder::new();
        want.write_string(&pool.name);
        want.write_opaque_fixed(&pool.uuid);
        want.write_u32(0);
        assert_eq!(encode_storage_pool_refresh_args(&pool), want.into_bytes());
    }

    #[test]
    fn decodes_a_storage_pool_reply() {
        let pool = sample_pool();
        let mut e = Encoder::new();
        e.write_string(&pool.name);
        e.write_opaque_fixed(&pool.uuid);
        assert_eq!(decode_storage_pool_ret(e.as_bytes()).unwrap(), pool);
    }

    /// For a directory pool the volume's `key` is its absolute path on the
    /// host — which is exactly what domain XML needs, so no pool-XML parsing
    /// is required anywhere.
    #[test]
    fn decodes_a_storage_vol_reply_carrying_its_path() {
        let mut e = Encoder::new();
        e.write_string("default");
        e.write_string("disk.qcow2");
        e.write_string("/var/lib/libvirt/images/disk.qcow2");
        let vol = decode_storage_vol_ret(e.as_bytes()).unwrap();
        assert_eq!(vol.key, "/var/lib/libvirt/images/disk.qcow2");
    }

    // ------------------------------------------------------------------
    // "Not found" is a normal answer, not a failure
    // ------------------------------------------------------------------

    /// libvirt reports a missing object as an error reply, not an empty one,
    /// so a reconciler asking "does this exist?" has to read the code. Three
    /// codes, transcribed from `virterror.h`.
    #[test]
    fn recognises_libvirt_not_found_codes() {
        for code in [
            VIR_ERR_NO_DOMAIN,
            VIR_ERR_NO_STORAGE_POOL,
            VIR_ERR_NO_STORAGE_VOL,
        ] {
            let err = TransportError::Remote {
                code,
                message: "not found".into(),
            };
            assert!(is_not_found(&err), "code {code} should read as not-found");
        }
    }

    #[test]
    fn not_found_codes_match_libvirt() {
        assert_eq!(VIR_ERR_NO_DOMAIN, 42);
        assert_eq!(VIR_ERR_NO_STORAGE_POOL, 49);
        assert_eq!(VIR_ERR_NO_STORAGE_VOL, 50);
    }

    /// Any other remote error is a real failure. Swallowing one as "absent"
    /// would make a reconciler recreate an object that already exists, or
    /// report a permission problem as an empty pool.
    #[test]
    fn other_errors_are_not_not_found() {
        let other = TransportError::Remote {
            code: 55,
            message: "operation failed".into(),
        };
        assert!(!is_not_found(&other));
        assert!(!is_not_found(&TransportError::Protocol {
            detail: "x".into()
        }));
        assert!(!is_not_found(&TransportError::Tls("handshake".into())));
    }

    // ------------------------------------------------------------------
    // DHCP leases — the hostname a guest announced
    // ------------------------------------------------------------------

    /// `remote_nonnull_network` is name + uuid, then `mac` as an *optional*
    /// string. Writing the optional's boolean wrongly shifts everything
    /// after it, which is the trap this module's own doc opens with.
    #[test]
    fn encodes_network_get_dhcp_leases_args() {
        let net = Network {
            name: "default".into(),
            uuid: uuid_bytes(0x50),
        };
        let mut want = Encoder::new();
        want.write_string(&net.name);
        want.write_opaque_fixed(&net.uuid);
        want.write_bool(false); // mac: None
        want.write_i32(1); // need_results
        want.write_u32(0); // flags
        assert_eq!(
            encode_network_get_dhcp_leases_args(&net, None),
            want.into_bytes()
        );
    }

    #[test]
    fn encodes_a_mac_filter_when_given() {
        let net = Network {
            name: "default".into(),
            uuid: uuid_bytes(1),
        };
        let mut want = Encoder::new();
        want.write_string(&net.name);
        want.write_opaque_fixed(&net.uuid);
        want.write_bool(true);
        want.write_string("52:54:00:aa:bb:cc");
        want.write_i32(1);
        want.write_u32(0);
        assert_eq!(
            encode_network_get_dhcp_leases_args(&net, Some("52:54:00:aa:bb:cc")),
            want.into_bytes()
        );
    }

    /// Build a `remote_network_get_dhcp_leases_ret` payload.
    fn lease_payload(leases: &[(&str, &str, Option<&str>)]) -> Vec<u8> {
        let mut e = Encoder::new();
        e.write_u32(leases.len() as u32);
        for (iface, ip, hostname) in leases {
            e.write_string(iface);
            e.write_i64(0); // expirytime
            e.write_i32(0); // type: ipv4
            e.write_bool(false); // mac
            e.write_bool(false); // iaid
            e.write_string(ip);
            e.write_u32(24); // prefix
            match hostname {
                Some(h) => {
                    e.write_bool(true);
                    e.write_string(h);
                }
                None => e.write_bool(false),
            }
            e.write_bool(false); // clientid
        }
        e.write_u32(leases.len() as u32); // trailing ret
        e.into_bytes()
    }

    /// The hostname is what makes a lease worth reading here: it is the
    /// name the *guest* announced, so it is evidence the guest's own
    /// configuration ran.
    #[test]
    fn decodes_a_lease_with_its_hostname() {
        let body = lease_payload(&[("virbr0", "192.0.2.42", Some("banlieue-seedcheck"))]);
        let leases = decode_network_get_dhcp_leases_ret(&body).unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].ipaddr, "192.0.2.42");
        assert_eq!(leases[0].hostname.as_deref(), Some("banlieue-seedcheck"));
    }

    /// A guest that sent no hostname is normal, not an error.
    #[test]
    fn decodes_a_lease_without_a_hostname() {
        let body = lease_payload(&[("virbr0", "192.0.2.43", None)]);
        let leases = decode_network_get_dhcp_leases_ret(&body).unwrap();
        assert!(leases[0].hostname.is_none());
    }

    #[test]
    fn decodes_several_leases_in_order() {
        let body = lease_payload(&[
            ("virbr0", "192.0.2.10", Some("a")),
            ("virbr0", "192.0.2.11", Some("b")),
        ]);
        let leases = decode_network_get_dhcp_leases_ret(&body).unwrap();
        assert_eq!(leases.len(), 2);
        assert_eq!(leases[1].hostname.as_deref(), Some("b"));
    }

    #[test]
    fn rejects_an_oversized_lease_count() {
        let mut e = Encoder::new();
        e.write_u32(NETWORK_DHCP_LEASES_MAX as u32 + 1);
        let err = decode_network_get_dhcp_leases_ret(e.as_bytes()).unwrap_err();
        assert!(matches!(err, TransportError::Protocol { .. }), "{err:?}");
    }

    // ------------------------------------------------------------------
    // qemu-guest-agent: reading the installed-guest marker (ADR-0043)
    // ------------------------------------------------------------------

    fn a_domain() -> Domain {
        Domain {
            name: "ns-vm".to_string(),
            uuid: [7u8; UUID_LEN],
            id: -1,
        }
    }

    /// `qemu_domain_agent_command_args { domain; string cmd; int timeout; uint flags; }`
    #[test]
    fn agent_command_args_encode_domain_then_command() {
        let args = encode_domain_qemu_agent_command_args(
            &a_domain(),
            r#"{"execute":"guest-ping"}"#,
            AGENT_TIMEOUT_DEFAULT,
            0,
        );
        let mut d = Decoder::new(&args);
        assert_eq!(d.read_string().unwrap(), "ns-vm");
        assert_eq!(d.read_opaque_fixed(UUID_LEN).unwrap(), &[7u8; UUID_LEN]);
        assert_eq!(d.read_i32().unwrap(), -1);
        assert_eq!(d.read_string().unwrap(), r#"{"execute":"guest-ping"}"#);
        assert_eq!(d.read_i32().unwrap(), AGENT_TIMEOUT_DEFAULT);
        assert_eq!(d.read_u32().unwrap(), 0);
    }

    /// The reply is an *optional* string: libvirt sends a pointer flag then
    /// the value. A command that returns nothing sends only the flag, and
    /// decoding that as a string would desynchronise the stream.
    #[test]
    fn agent_command_reply_decodes_an_optional_string() {
        let mut e = Encoder::new();
        e.write_bool(true);
        e.write_string(r#"{"return":{}}"#);
        assert_eq!(
            decode_domain_qemu_agent_command_ret(&e.into_bytes()).unwrap(),
            Some(r#"{"return":{}}"#.to_string())
        );

        let mut e = Encoder::new();
        e.write_bool(false);
        assert_eq!(
            decode_domain_qemu_agent_command_ret(&e.into_bytes()).unwrap(),
            None
        );
    }
}
