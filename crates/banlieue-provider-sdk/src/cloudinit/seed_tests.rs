// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the NoCloud seed builder.

#[cfg(test)]
mod tests {
    use super::super::*;

    const UUID: &str = "0f3c9a1e-0000-4000-8000-000000000001";

    /// cloud-init requires `meta-data` even when there is no user-data, and
    /// a seed without it is silently ignored rather than rejected.
    #[test]
    fn meta_data_is_always_present() {
        let files = seed_files("vm-01", UUID, None);
        assert!(files.iter().any(|f| f.name == "meta-data"));
    }

    /// `instance-id` is what makes cloud-init re-run its per-instance
    /// modules for a genuinely new VM instead of treating first boot as a
    /// reboot. Deriving it from the domain UUID means a rebuilt pool member
    /// is a new instance by construction (ADR-0054 Decision 5).
    #[test]
    fn instance_id_comes_from_the_domain_uuid() {
        let files = seed_files("vm-01", UUID, None);
        let meta = files.iter().find(|f| f.name == "meta-data").unwrap();
        let text = String::from_utf8(meta.data.clone()).unwrap();
        assert!(text.contains(&format!("instance-id: {UUID}")), "{text}");
    }

    #[test]
    fn local_hostname_comes_from_the_domain_name() {
        let files = seed_files("web-01", UUID, None);
        let meta = files.iter().find(|f| f.name == "meta-data").unwrap();
        let text = String::from_utf8(meta.data.clone()).unwrap();
        assert!(text.contains("local-hostname: web-01"), "{text}");
    }

    /// Two VMs from the same image must not share an instance-id, or the
    /// second one's cloud-init treats its first boot as a reboot and skips
    /// every per-instance module.
    #[test]
    fn two_domains_get_distinct_instance_ids() {
        let a = seed_files("a", "uuid-a", None);
        let b = seed_files("b", "uuid-b", None);
        assert_ne!(a[0].data, b[0].data);
    }

    #[test]
    fn user_data_is_carried_verbatim() {
        let payload = "#cloud-config\nruncmd:\n  - [echo, hi]\n";
        let files = seed_files("vm-01", UUID, Some(payload));
        let ud = files.iter().find(|f| f.name == "user-data").unwrap();
        assert_eq!(ud.data, payload.as_bytes());
    }

    /// No user-data is a legitimate state — the guest still wants its
    /// hostname and instance-id — so the seed is built with meta-data alone
    /// rather than skipped.
    #[test]
    fn a_seed_without_user_data_still_builds() {
        let files = seed_files("vm-01", UUID, None);
        assert!(!files.iter().any(|f| f.name == "user-data"));
        assert!(build_seed_iso("vm-01", UUID, None).is_ok());
    }

    /// The whole point: the finished image is what cloud-init mounts.
    #[test]
    fn the_built_image_carries_the_cidata_label_and_both_files() {
        let img = build_seed_iso("vm-01", UUID, Some("#cloud-config\n")).unwrap();
        assert_eq!(&img[16 * 2048 + 40..16 * 2048 + 46], b"CIDATA");
        for name in ["meta-data", "user-data"] {
            let ucs2: Vec<u8> = name.encode_utf16().flat_map(u16::to_be_bytes).collect();
            assert!(
                img.windows(ucs2.len()).any(|w| w == ucs2),
                "{name} missing from the Joliet tree"
            );
        }
    }

    /// A reconciler rebuilds this every pass; if it churned it would
    /// rewrite the volume on the host forever.
    #[test]
    fn the_seed_is_deterministic() {
        let a = build_seed_iso("vm-01", UUID, Some("#cloud-config\n")).unwrap();
        let b = build_seed_iso("vm-01", UUID, Some("#cloud-config\n")).unwrap();
        assert_eq!(a, b);
    }

    /// Write a seed image to `BANLIEUE_SEED_OUT` so it can be mounted by an
    /// independent ISO9660 implementation.
    ///
    /// The offline tests above read our own bytes back and can only prove
    /// self-consistency — a misunderstanding of the format passes all of
    /// them. Mounting the result somewhere that did not produce it is the
    /// cheapest way to find out whether `meta-data` and `user-data` are
    /// really the names a guest sees (ADR-0054 Decision 3).
    ///
    /// ```sh
    /// BANLIEUE_SEED_OUT=/tmp/seed.iso cargo test -p banlieue-provider-libvirt \
    ///   --lib writes_a_seed_for_external_inspection -- --ignored
    /// hdiutil attach /tmp/seed.iso      # macOS
    /// mount -o loop /tmp/seed.iso /mnt  # Linux
    /// ```
    #[test]
    #[ignore = "writes a file; set BANLIEUE_SEED_OUT"]
    fn writes_a_seed_for_external_inspection() {
        let path = std::env::var("BANLIEUE_SEED_OUT").expect("set BANLIEUE_SEED_OUT");
        let img = build_seed_iso(
            "banlieue-seedcheck",
            UUID,
            Some("#cloud-config\nhostname: banlieue-seedcheck\n"),
        )
        .expect("build");
        std::fs::write(&path, &img).expect("write");
        eprintln!("wrote {} bytes to {path}", img.len());
    }
}
