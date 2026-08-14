// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//
// Fuzzes the libvirt RPC wire decoder with arbitrary bytes. The decoder runs
// on data read off a (TLS-authenticated, but still network-provided) socket,
// so it must never panic, overflow, or read out of bounds — returning an
// error for malformed input is always fine.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Length-prefix + message-header + payload bounds handling.
    let _ = banlieue_libvirt::decode_message(data);

    // Raw XDR scalar/compound reads off the same bytes.
    let mut d = banlieue_libvirt::Decoder::new(data);
    while !d.is_empty() {
        if d.read_opaque_var().is_err() {
            break;
        }
        if d.read_i64().is_err() {
            break;
        }
        if d.read_string().is_err() {
            break;
        }
    }
});
