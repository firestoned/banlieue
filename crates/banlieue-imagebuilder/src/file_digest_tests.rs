// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `file_digest.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;

    /// FIPS 180-2 test vector: SHA-256("abc").
    const ABC_SHA256: &str =
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    /// FIPS 180-2 test vector: SHA-512("abc").
    const ABC_SHA512: &str = "sha512:ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f";

    #[test]
    fn sha256_of_a_known_input_matches_the_standard_vector() {
        let d = digest_reader(&b"abc"[..], false).unwrap();
        assert_eq!(d.sha256, ABC_SHA256);
        assert_eq!(d.sha512, None);
    }

    #[test]
    fn sha512_is_computed_only_when_asked_for() {
        let d = digest_reader(&b"abc"[..], true).unwrap();
        assert_eq!(d.sha256, ABC_SHA256);
        assert_eq!(d.sha512.as_deref(), Some(ABC_SHA512));
    }

    /// A file larger than the read buffer hashes the same as hashing it in
    /// one go: streaming loses nothing at the buffer boundaries.
    #[test]
    fn streaming_across_buffer_boundaries_matches_a_one_shot_hash() {
        let data: Vec<u8> = (0..READ_BUFFER_BYTES * 2 + 7)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let d = digest_reader(&data[..], false).unwrap();
        assert_eq!(d.sha256, banlieue_oci::manifest::sha256_digest(&data));
    }

    #[test]
    fn the_message_is_sha256_first_then_sha512_one_per_line() {
        let d = digest_reader(&b"abc"[..], true).unwrap();
        assert_eq!(d.to_message(), format!("{ABC_SHA256}\n{ABC_SHA512}"));
        let d = digest_reader(&b"abc"[..], false).unwrap();
        assert_eq!(d.to_message(), ABC_SHA256);
    }

    #[test]
    fn run_writes_the_message_to_the_termination_log() {
        let dir = std::env::temp_dir().join(format!("banlieue-digest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("disk.raw");
        let log = dir.join("termination-log");
        std::fs::write(&file, b"abc").unwrap();

        run_blocking(&DigestArgs {
            file: file.clone(),
            sha512: false,
            termination_log: log.clone(),
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), ABC_SHA256);

        let missing = run_blocking(&DigestArgs {
            file: dir.join("absent.raw"),
            sha512: false,
            termination_log: log,
        });
        assert!(missing.is_err(), "an unreadable file fails the Job");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
