// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `rpc.rs`.
//!
//! As with the XDR tests, assertions pin the **exact bytes** rather than only
//! round-tripping. Field order and enum values here come from libvirt's
//! `virnetprotocol.x`; a self-consistent mistake would round-trip perfectly
//! and desynchronise on first contact with a real libvirtd.

#[cfg(test)]
mod tests {
    use super::super::*;

    /// A header for `CONNECT_OPEN`, the first message any session sends.
    fn open_call_header() -> MessageHeader {
        MessageHeader {
            program: REMOTE_PROGRAM,
            version: REMOTE_PROTOCOL_VERSION,
            procedure: PROC_CONNECT_OPEN,
            message_type: MessageType::Call,
            serial: 1,
            status: MessageStatus::Ok,
        }
    }

    // ------------------------------------------------------------------
    // Constants, checked against virnetprotocol.x / remote_protocol.x
    // ------------------------------------------------------------------

    #[test]
    fn protocol_constants_match_libvirt_headers() {
        assert_eq!(REMOTE_PROGRAM, 0x2000_8086);
        assert_eq!(REMOTE_PROTOCOL_VERSION, 1);
        assert_eq!(MESSAGE_HEADER_LEN, 24);
        assert_eq!(MESSAGE_LEN_PREFIX_LEN, 4);
        assert_eq!(MESSAGE_MAX, 33_554_432);
        assert_eq!(PAYLOAD_MAX, MESSAGE_MAX - MESSAGE_HEADER_LEN);
    }

    // ------------------------------------------------------------------
    // Enums — values are wire-visible, so pin every one
    // ------------------------------------------------------------------

    #[test]
    fn message_type_values_match_virnetmessagetype() {
        for (v, expected) in [
            (MessageType::Call, 0),
            (MessageType::Reply, 1),
            (MessageType::Message, 2),
            (MessageType::Stream, 3),
            (MessageType::CallWithFds, 4),
            (MessageType::ReplyWithFds, 5),
            (MessageType::StreamHole, 6),
        ] {
            assert_eq!(v as i32, expected, "{v:?}");
            assert_eq!(MessageType::from_wire(expected).unwrap(), v);
        }
    }

    #[test]
    fn message_status_values_match_virnetmessagestatus() {
        for (v, expected) in [
            (MessageStatus::Ok, 0),
            (MessageStatus::Error, 1),
            (MessageStatus::Continue, 2),
        ] {
            assert_eq!(v as i32, expected, "{v:?}");
            assert_eq!(MessageStatus::from_wire(expected).unwrap(), v);
        }
    }

    #[test]
    fn unknown_enum_values_are_rejected_not_guessed() {
        // A desynchronised stream yields arbitrary words here. Failing loudly
        // beats silently treating an unknown type as a Call.
        assert!(matches!(
            MessageType::from_wire(7),
            Err(RpcError::InvalidMessageType(7))
        ));
        assert!(matches!(
            MessageType::from_wire(-1),
            Err(RpcError::InvalidMessageType(-1))
        ));
        assert!(matches!(
            MessageStatus::from_wire(3),
            Err(RpcError::InvalidMessageStatus(3))
        ));
    }

    // ------------------------------------------------------------------
    // Header layout
    // ------------------------------------------------------------------

    #[test]
    fn header_encodes_to_exactly_twenty_four_bytes_in_declared_field_order() {
        // virNetMessageHeader { prog, vers, proc, type, serial, status }
        let encoded = open_call_header().encode();
        assert_eq!(encoded.len(), MESSAGE_HEADER_LEN);
        assert_eq!(
            encoded,
            vec![
                0x20, 0x00, 0x80, 0x86, // prog   = 0x20008086
                0x00, 0x00, 0x00, 0x01, // vers   = 1
                0x00, 0x00, 0x00, 0x01, // proc   = 1 (CONNECT_OPEN)
                0x00, 0x00, 0x00, 0x00, // type   = VIR_NET_CALL
                0x00, 0x00, 0x00, 0x01, // serial = 1
                0x00, 0x00, 0x00, 0x00, // status = VIR_NET_OK
            ]
        );
    }

    #[test]
    fn header_round_trips() {
        let h = open_call_header();
        let encoded = h.encode();
        let mut d = Decoder::new(&encoded);
        assert_eq!(MessageHeader::decode(&mut d).unwrap(), h);
    }

    #[test]
    fn procedure_is_signed() {
        // `proc` is declared `int`, not `unsigned`, in virnetprotocol.x.
        let h = MessageHeader {
            procedure: -5,
            ..open_call_header()
        };
        let encoded = h.encode();
        let mut d = Decoder::new(&encoded);
        assert_eq!(MessageHeader::decode(&mut d).unwrap().procedure, -5);
    }

    #[test]
    fn header_decode_rejects_truncated_input() {
        let bytes = [0u8; MESSAGE_HEADER_LEN - 1];
        let mut d = Decoder::new(&bytes);
        assert!(MessageHeader::decode(&mut d).is_err());
    }

    // ------------------------------------------------------------------
    // Framing — the length prefix INCLUDES itself
    // ------------------------------------------------------------------

    #[test]
    fn encoded_message_length_prefix_counts_itself_and_the_header() {
        let msg = encode_message(&open_call_header(), &[]);
        assert_eq!(msg.len(), MESSAGE_LEN_PREFIX_LEN + MESSAGE_HEADER_LEN);
        let prefix = u32::from_be_bytes(msg[0..4].try_into().unwrap()) as usize;
        assert_eq!(
            prefix,
            msg.len(),
            "prefix must be the TOTAL length, including the prefix word"
        );
        assert_eq!(prefix, 28);
    }

    #[test]
    fn encoded_message_carries_its_payload() {
        let payload = [0xDE, 0xAD, 0xBE, 0xEF];
        let msg = encode_message(&open_call_header(), &payload);
        assert_eq!(
            msg.len(),
            MESSAGE_LEN_PREFIX_LEN + MESSAGE_HEADER_LEN + payload.len()
        );
        assert_eq!(
            &msg[MESSAGE_LEN_PREFIX_LEN + MESSAGE_HEADER_LEN..],
            &payload
        );
    }

    #[test]
    fn decode_message_round_trips_header_and_payload() {
        let payload = [1u8, 2, 3, 4];
        let msg = encode_message(&open_call_header(), &payload);
        let (header, body) = decode_message(&msg).unwrap();
        assert_eq!(header, open_call_header());
        assert_eq!(body, &payload);
    }

    // ------------------------------------------------------------------
    // Length-prefix validation — the security-relevant cases
    // ------------------------------------------------------------------

    #[test]
    fn parse_length_rejects_a_prefix_below_the_minimum_message() {
        // Smaller than prefix + header: cannot possibly be a valid message.
        for bad in [0u32, 4, 27] {
            assert!(
                matches!(
                    parse_length_prefix(&bad.to_be_bytes()),
                    Err(RpcError::InvalidLength(_))
                ),
                "length {bad} should be rejected"
            );
        }
    }

    #[test]
    fn parse_length_rejects_a_prefix_above_the_protocol_maximum() {
        // VIR_NET_MESSAGE_MAX is the cap libvirtd itself enforces. Rejecting
        // here means a hostile prefix never becomes a 4 GiB allocation.
        let too_big = (MESSAGE_MAX + 1) as u32;
        assert!(matches!(
            parse_length_prefix(&too_big.to_be_bytes()),
            Err(RpcError::InvalidLength(_))
        ));
        assert!(matches!(
            parse_length_prefix(&u32::MAX.to_be_bytes()),
            Err(RpcError::InvalidLength(_))
        ));
    }

    #[test]
    fn parse_length_accepts_the_boundaries() {
        let min = (MESSAGE_LEN_PREFIX_LEN + MESSAGE_HEADER_LEN) as u32;
        assert_eq!(
            parse_length_prefix(&min.to_be_bytes()).unwrap(),
            min as usize
        );
        let max = MESSAGE_MAX as u32;
        assert_eq!(
            parse_length_prefix(&max.to_be_bytes()).unwrap(),
            max as usize
        );
    }

    #[test]
    fn decode_message_rejects_a_frame_shorter_than_its_prefix_claims() {
        let mut msg = encode_message(&open_call_header(), &[1, 2, 3, 4]);
        msg.pop(); // truncate the payload
        assert!(matches!(
            decode_message(&msg),
            Err(RpcError::Truncated { .. })
        ));
    }

    #[test]
    fn decode_message_rejects_input_too_short_for_a_prefix() {
        assert!(decode_message(&[0, 0]).is_err());
    }

    // ------------------------------------------------------------------
    // Error replies
    // ------------------------------------------------------------------

    #[test]
    fn error_status_is_visible_on_the_decoded_header() {
        let header = MessageHeader {
            message_type: MessageType::Reply,
            status: MessageStatus::Error,
            ..open_call_header()
        };
        let msg = encode_message(&header, &[]);
        let (decoded, _) = decode_message(&msg).unwrap();
        assert_eq!(decoded.status, MessageStatus::Error);
        assert_eq!(decoded.message_type, MessageType::Reply);
    }
}
