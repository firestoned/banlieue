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
}
