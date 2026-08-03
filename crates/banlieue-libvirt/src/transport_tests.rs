// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `transport.rs`.
//!
//! [`Session`] is generic over the stream, so these drive the real protocol
//! logic over `tokio::io::duplex` with a scripted peer — no socket, no TLS, no
//! libvirtd. What is NOT covered here is `connect_tls`, which needs a live
//! endpoint; that is the integration test ADR-0011 records as non-optional.

#[cfg(test)]
mod tests {
    // `super::super::*` is the `transport` module, which imports only what it
    // itself needs; the scripted peer below needs a few more protocol items.
    use super::super::*;
    use crate::rpc::{PROC_CONNECT_OPEN, decode_message};
    use crate::xdr::Encoder;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Encode a reply the way libvirtd would, for a scripted peer to send.
    fn reply(serial: u32, status: MessageStatus, payload: &[u8]) -> Vec<u8> {
        encode_message(
            &MessageHeader {
                program: REMOTE_PROGRAM,
                version: REMOTE_PROTOCOL_VERSION,
                procedure: PROC_CONNECT_OPEN,
                message_type: MessageType::Reply,
                serial,
                status,
            },
            payload,
        )
    }

    /// A `virNetMessageError` payload: code, domain, then an optional message.
    fn error_payload(code: i32, message: &str) -> Vec<u8> {
        let mut e = Encoder::new();
        e.write_i32(code);
        e.write_i32(0); // domain
        e.write_bool(true); // message pointer is present
        e.write_string(message);
        e.into_bytes()
    }

    #[tokio::test]
    async fn send_writes_a_correctly_framed_message() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let mut session = Session::new(client);

        let header = MessageHeader {
            program: REMOTE_PROGRAM,
            version: REMOTE_PROTOCOL_VERSION,
            procedure: PROC_CONNECT_OPEN,
            message_type: MessageType::Call,
            serial: 1,
            status: MessageStatus::Ok,
        };
        session
            .send(&header, &[0xAA, 0xBB, 0xCC, 0xDD])
            .await
            .unwrap();

        let mut buf = vec![0u8; 32];
        peer.read_exact(&mut buf).await.unwrap();
        let (decoded, payload) = decode_message(&buf).unwrap();
        assert_eq!(decoded, header);
        assert_eq!(payload, &[0xAA, 0xBB, 0xCC, 0xDD]);
    }

    #[tokio::test]
    async fn call_round_trips_against_a_scripted_peer() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let mut session = Session::new(client);

        let server = tokio::spawn(async move {
            // Read the call and echo a successful reply with the same serial.
            let mut prefix = [0u8; 4];
            peer.read_exact(&mut prefix).await.unwrap();
            let total = u32::from_be_bytes(prefix) as usize;
            let mut rest = vec![0u8; total - 4];
            peer.read_exact(&mut rest).await.unwrap();

            let mut framed = prefix.to_vec();
            framed.extend_from_slice(&rest);
            let (header, _) = decode_message(&framed).unwrap();
            assert_eq!(header.message_type, MessageType::Call);
            assert_eq!(header.program, REMOTE_PROGRAM);

            peer.write_all(&reply(header.serial, MessageStatus::Ok, b"pong"))
                .await
                .unwrap();
        });

        let body = session.call(PROC_CONNECT_OPEN, b"ping").await.unwrap();
        assert_eq!(body, b"pong");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn call_surfaces_a_remote_error_with_code_and_message() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let mut session = Session::new(client);

        tokio::spawn(async move {
            let mut prefix = [0u8; 4];
            peer.read_exact(&mut prefix).await.unwrap();
            let total = u32::from_be_bytes(prefix) as usize;
            let mut rest = vec![0u8; total - 4];
            peer.read_exact(&mut rest).await.unwrap();
            peer.write_all(&reply(
                0,
                MessageStatus::Error,
                &error_payload(38, "Storage pool not found"),
            ))
            .await
            .unwrap();
        });

        let err = session.call(PROC_CONNECT_OPEN, b"").await.unwrap_err();
        match err {
            TransportError::Remote { code, message } => {
                assert_eq!(code, 38);
                assert_eq!(message, "Storage pool not found");
            }
            other => panic!("expected Remote, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn call_rejects_a_reply_with_the_wrong_serial() {
        // One in-flight call at a time means a serial mismatch is a
        // desynchronised stream, not a message to skip.
        let (client, mut peer) = tokio::io::duplex(4096);
        let mut session = Session::new(client);

        tokio::spawn(async move {
            let mut prefix = [0u8; 4];
            peer.read_exact(&mut prefix).await.unwrap();
            let total = u32::from_be_bytes(prefix) as usize;
            let mut rest = vec![0u8; total - 4];
            peer.read_exact(&mut rest).await.unwrap();
            peer.write_all(&reply(99, MessageStatus::Ok, b""))
                .await
                .unwrap();
        });

        let err = session.call(PROC_CONNECT_OPEN, b"").await.unwrap_err();
        assert!(matches!(
            err,
            TransportError::SerialMismatch {
                expected: 0,
                got: 99
            }
        ));
    }

    #[tokio::test]
    async fn call_rejects_a_non_reply_message_type() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let mut session = Session::new(client);

        tokio::spawn(async move {
            let mut prefix = [0u8; 4];
            peer.read_exact(&mut prefix).await.unwrap();
            let total = u32::from_be_bytes(prefix) as usize;
            let mut rest = vec![0u8; total - 4];
            peer.read_exact(&mut rest).await.unwrap();
            let msg = encode_message(
                &MessageHeader {
                    program: REMOTE_PROGRAM,
                    version: REMOTE_PROTOCOL_VERSION,
                    procedure: PROC_CONNECT_OPEN,
                    message_type: MessageType::Message, // an async event
                    serial: 0,
                    status: MessageStatus::Ok,
                },
                b"",
            );
            peer.write_all(&msg).await.unwrap();
        });

        let err = session.call(PROC_CONNECT_OPEN, b"").await.unwrap_err();
        assert!(matches!(
            err,
            TransportError::UnexpectedMessageType(MessageType::Message)
        ));
    }

    #[tokio::test]
    async fn serials_increment_per_call() {
        let (client, mut peer) = tokio::io::duplex(8192);
        let mut session = Session::new(client);

        let server = tokio::spawn(async move {
            let mut seen = Vec::new();
            for _ in 0..3 {
                let mut prefix = [0u8; 4];
                peer.read_exact(&mut prefix).await.unwrap();
                let total = u32::from_be_bytes(prefix) as usize;
                let mut rest = vec![0u8; total - 4];
                peer.read_exact(&mut rest).await.unwrap();
                let mut framed = prefix.to_vec();
                framed.extend_from_slice(&rest);
                let (h, _) = decode_message(&framed).unwrap();
                seen.push(h.serial);
                peer.write_all(&reply(h.serial, MessageStatus::Ok, b""))
                    .await
                    .unwrap();
            }
            seen
        });

        for _ in 0..3 {
            session.call(PROC_CONNECT_OPEN, b"").await.unwrap();
        }
        // libvirt's own client starts at 0; verified against a live trace.
        assert_eq!(server.await.unwrap(), vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn recv_reassembles_a_message_split_across_writes() {
        // Real sockets deliver partial messages. `read_exact` must reassemble
        // rather than treat a short read as a truncated message.
        let (client, mut peer) = tokio::io::duplex(4096);
        let mut session = Session::new(client);

        tokio::spawn(async move {
            let msg = reply(1, MessageStatus::Ok, b"split");
            let (head, tail) = msg.split_at(7); // mid-header
            peer.write_all(head).await.unwrap();
            tokio::task::yield_now().await;
            peer.write_all(tail).await.unwrap();
        });

        let (header, payload) = session.recv().await.unwrap();
        assert_eq!(header.serial, 1);
        assert_eq!(payload, b"split");
    }

    #[tokio::test]
    async fn recv_rejects_an_oversized_length_prefix_without_allocating() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let mut session = Session::new(client);

        tokio::spawn(async move {
            // Well past VIR_NET_MESSAGE_MAX; must be refused on the prefix
            // alone rather than used to size a buffer.
            peer.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
        });

        let err = session.recv().await.unwrap_err();
        assert!(matches!(
            err,
            TransportError::Rpc(RpcError::InvalidLength(_))
        ));
    }

    #[tokio::test]
    async fn recv_reports_io_error_when_the_peer_hangs_up_mid_message() {
        let (client, peer) = tokio::io::duplex(4096);
        let mut session = Session::new(client);
        drop(peer); // closed before anything is written
        assert!(matches!(session.recv().await, Err(TransportError::Io(_))));
    }

    #[tokio::test]
    async fn call_times_out_when_the_peer_never_replies() {
        // The failure this guards against was real: a live libvirtd accepted
        // the connection, answered nothing, and the client blocked forever.
        // In a controller that stalls every other reconcile.
        let (client, _peer) = tokio::io::duplex(4096);
        let mut session = Session::with_timeout(client, std::time::Duration::from_millis(150));
        let err = session.call(PROC_CONNECT_OPEN, b"").await.unwrap_err();
        assert!(
            matches!(err, TransportError::Timeout { op: "receive", .. }),
            "expected receive Timeout, got {err:?}"
        );
    }

    #[tokio::test]
    async fn send_times_out_when_the_peer_never_reads() {
        // The write-side twin of the hang above: a peer that stops reading
        // fills the socket buffer, and an un-timed-out `write_all` would
        // block there forever (SEC-012).
        let (client, _peer) = tokio::io::duplex(64);
        let mut session = Session::with_timeout(client, std::time::Duration::from_millis(150));
        let header = MessageHeader {
            program: REMOTE_PROGRAM,
            version: REMOTE_PROTOCOL_VERSION,
            procedure: PROC_CONNECT_OPEN,
            message_type: MessageType::Call,
            serial: 0,
            status: MessageStatus::Ok,
        };
        // Far larger than the 64-byte duplex buffer: the write must stall.
        let err = session.send(&header, &[0u8; 4096]).await.unwrap_err();
        assert!(
            matches!(err, TransportError::Timeout { op: "send", .. }),
            "expected send Timeout, got {err:?}"
        );
    }

    #[tokio::test]
    async fn recv_rejects_a_reply_from_a_different_program() {
        // A desynchronised stream reads a header at the wrong offset, which
        // shows up first as a nonsense program/version. Catching it here turns
        // a would-be indefinite block into a clear error.
        let (client, mut peer) = tokio::io::duplex(4096);
        let mut session = Session::new(client);

        tokio::spawn(async move {
            let msg = encode_message(
                &MessageHeader {
                    program: 0xDEAD_BEEF,
                    version: REMOTE_PROTOCOL_VERSION,
                    procedure: PROC_CONNECT_OPEN,
                    message_type: MessageType::Reply,
                    serial: 0,
                    status: MessageStatus::Ok,
                },
                b"",
            );
            peer.write_all(&msg).await.unwrap();
        });

        let err = session.recv().await.unwrap_err();
        assert!(
            matches!(
                err,
                TransportError::Desynchronised {
                    got_program: 0xDEAD_BEEF,
                    ..
                }
            ),
            "expected Desynchronised, got {err:?}"
        );
    }
}
