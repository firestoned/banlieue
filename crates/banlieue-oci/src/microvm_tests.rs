// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `microvm.rs`: the pure parts directly, and push and pull
//! against an in-memory registry served on loopback (no network, no real
//! registry). Every fail-closed condition of ADR-0142 Decision 6 has its own
//! test, and each asserts that nothing is left in the image area.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::{Arc, Mutex};

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::super::*;
    use crate::client::Client;
    use crate::manifest::{
        ARTIFACT_TYPE_RAW, LAYER_MEDIA_TYPE_GZIP, MANIFEST_MEDIA_TYPE, Manifest, sha256_digest,
    };

    const KIB: usize = 1024;
    const CEILING: u64 = 64 * 1024 * 1024;
    const HEADER_END: &[u8] = b"\r\n\r\n";
    const READ_CHUNK: usize = 16 * KIB;
    const MODE_MASK: u32 = 0o777;
    const DIR_MODE: u32 = 0o755;
    const FILE_MODE: u32 = 0o644;

    // ---------------------------------------------------------------- fake registry

    #[derive(Default)]
    struct State {
        blobs: HashMap<String, Vec<u8>>,
        manifests: HashMap<String, Vec<u8>>,
    }

    /// An in-memory registry: just the calls the client makes.
    struct Fake {
        state: Arc<Mutex<State>>,
        addr: String,
    }

    impl Fake {
        async fn start() -> Self {
            crate::install_crypto_provider();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let state = Arc::new(Mutex::new(State::default()));
            let shared = state.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((conn, _)) = listener.accept().await else {
                        return;
                    };
                    tokio::spawn(serve(conn, shared.clone()));
                }
            });
            Self { state, addr }
        }

        fn client(&self) -> Client {
            Client::new(
                reqwest::Client::new(),
                crate::Credentials {
                    username: None,
                    password: None,
                },
                true,
            )
        }

        fn reference(&self) -> Reference {
            Reference::parse(&format!("{}/sandbox/image", self.addr)).unwrap()
        }

        fn blob_count(&self) -> usize {
            self.state.lock().unwrap().blobs.len()
        }

        /// Store `bytes` as a blob under their own digest.
        fn put_blob(&self, bytes: &[u8]) -> String {
            let digest = sha256_digest(bytes);
            self.state
                .lock()
                .unwrap()
                .blobs
                .insert(digest.clone(), bytes.to_vec());
            digest
        }

        /// Store a blob under `digest` with different content (a lying registry).
        fn put_blob_as(&self, digest: &str, bytes: &[u8]) {
            self.state
                .lock()
                .unwrap()
                .blobs
                .insert(digest.to_string(), bytes.to_vec());
        }

        /// Store a manifest and return its reference by digest.
        fn put_manifest(&self, manifest: &Manifest) -> Reference {
            let body = serde_json::to_vec(manifest).unwrap();
            let digest = sha256_digest(&body);
            self.state
                .lock()
                .unwrap()
                .manifests
                .insert(digest.clone(), body);
            self.reference().with_digest(&digest)
        }
    }

    fn split_head(buf: &[u8]) -> Option<usize> {
        buf.windows(HEADER_END.len())
            .position(|w| w == HEADER_END)
            .map(|i| i + HEADER_END.len())
    }

    async fn respond(conn: &mut tokio::net::TcpStream, status: &str, headers: &str, body: &[u8]) {
        let head = format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = conn.write_all(head.as_bytes()).await;
        let _ = conn.write_all(body).await;
        let _ = conn.shutdown().await;
    }

    async fn serve(mut conn: tokio::net::TcpStream, state: Arc<Mutex<State>>) {
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; READ_CHUNK];
        let head_len = loop {
            if let Some(n) = split_head(&buf) {
                break n;
            }
            match conn.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_len]).into_owned();
        let mut lines = head.lines();
        let request = lines.next().unwrap_or_default().to_string();
        let content_length: usize = lines
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse().ok())
            .unwrap_or(0);
        while buf.len() < head_len + content_length {
            match conn.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let body = buf[head_len..head_len + content_length].to_vec();
        let mut parts = request.split_whitespace();
        let method = parts.next().unwrap_or_default();
        let target = parts.next().unwrap_or_default();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));

        let (status, headers, payload) = route(&state, method, path, query, &body);
        respond(&mut conn, status, &headers, &payload).await;
    }

    fn route(
        state: &Mutex<State>,
        method: &str,
        path: &str,
        query: &str,
        body: &[u8],
    ) -> (&'static str, String, Vec<u8>) {
        const NOT_FOUND: &str = "404 Not Found";
        let mut st = state.lock().unwrap();
        if let Some((_, digest)) = path.split_once("/blobs/sha256:") {
            let digest = format!("sha256:{digest}");
            return match (method, st.blobs.get(&digest)) {
                ("HEAD", Some(_)) => ("200 OK", String::new(), Vec::new()),
                ("GET", Some(b)) => ("200 OK", String::new(), b.clone()),
                _ => (NOT_FOUND, String::new(), Vec::new()),
            };
        }
        if method == "POST" && path.ends_with("/blobs/uploads/") {
            let loc = format!("Location: {path}session\r\n");
            return ("202 Accepted", loc, Vec::new());
        }
        if method == "PUT" && path.ends_with("/blobs/uploads/session") {
            let digest = query
                .strip_prefix("digest=")
                .unwrap_or_default()
                .to_string();
            if sha256_digest(body) != digest {
                return ("400 Bad Request", String::new(), Vec::new());
            }
            st.blobs.insert(digest, body.to_vec());
            return ("201 Created", String::new(), Vec::new());
        }
        if let Some((_, reference)) = path.split_once("/manifests/") {
            if method == "PUT" {
                let digest = sha256_digest(body);
                st.manifests.insert(digest.clone(), body.to_vec());
                st.manifests.insert(reference.to_string(), body.to_vec());
                return ("201 Created", String::new(), Vec::new());
            }
            if let (true, Some(m)) = (method == "GET", st.manifests.get(reference)) {
                return ("200 OK", String::new(), m.clone());
            }
        }
        (NOT_FOUND, String::new(), Vec::new())
    }

    // ---------------------------------------------------------------- fixtures

    fn gz(bytes: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(bytes).unwrap();
        e.finish().unwrap()
    }

    fn config(cmdline: &str) -> MicrovmConfig {
        MicrovmConfig {
            schema_version: CONFIG_SCHEMA_VERSION,
            architecture: Architecture::Amd64,
            cmdline: cmdline.into(),
            min_memory_mib: 256,
        }
    }

    fn config_bytes(c: &MicrovmConfig) -> Vec<u8> {
        serde_json::to_vec(c).unwrap()
    }

    fn desc(media_type: &str, bytes: &[u8], annotations: &[(&str, String)]) -> Descriptor {
        Descriptor {
            media_type: media_type.into(),
            digest: sha256_digest(bytes),
            size: bytes.len() as u64,
            annotations: annotations
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect(),
        }
    }

    fn gz_desc(media_type: &str, plain: &[u8]) -> Descriptor {
        desc(
            media_type,
            &gz(plain),
            &[(ANNOTATION_LAYER_SIZE, plain.len().to_string())],
        )
    }

    const KERNEL: &[u8] = b"\x7fELF pretend vmlinux";
    const INITRAMFS: &[u8] = b"070701 pretend cpio";
    const ROOTFS: &[u8] = b"hsqs pretend squashfs, stored as built";

    /// A well-formed image, published to `fake`, as a layout the caller may
    /// damage before it is turned into a manifest.
    fn good_layout(fake: &Fake, cfg: &MicrovmConfig, initramfs: bool) -> MicrovmLayout {
        let config_blob = config_bytes(cfg);
        fake.put_blob(&config_blob);
        let kernel = gz_desc(LAYER_KERNEL, KERNEL);
        fake.put_blob(&gz(KERNEL));
        let initramfs = initramfs.then(|| {
            fake.put_blob(&gz(INITRAMFS));
            gz_desc(LAYER_INITRAMFS, INITRAMFS)
        });
        fake.put_blob(ROOTFS);
        MicrovmLayout {
            config: desc(CONFIG_MEDIA_TYPE, &config_blob, &[]),
            kernel,
            initramfs,
            rootfs: desc(LAYER_ROOTFS, ROOTFS, &[]),
        }
    }

    fn publish(fake: &Fake, layout: &MicrovmLayout) -> Reference {
        fake.put_manifest(&build_manifest(layout))
    }

    fn entries(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    /// Pull must fail with `Error::Manifest` naming `needle`, leaving `images`
    /// empty.
    async fn assert_refused(
        fake: &Fake,
        r: &Reference,
        arch: Architecture,
        max: u64,
        needle: &str,
    ) {
        let images = tempfile::tempdir().unwrap();
        let err = fake
            .client()
            .pull_microvm(r, images.path(), arch, max)
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains(needle), "wanted {needle:?} in {text:?}");
        assert!(
            entries(images.path()).is_empty(),
            "left behind: {:?}",
            entries(images.path())
        );
    }

    async fn assert_refused_default(fake: &Fake, r: &Reference, needle: &str) {
        assert_refused(fake, r, Architecture::Amd64, CEILING, needle).await;
    }

    // ---------------------------------------------------------------- constants

    #[test]
    fn the_contract_strings_are_the_adr_s() {
        assert_eq!(ARTIFACT_TYPE, "application/vnd.banlieue.microvm.v1");
        assert_eq!(
            CONFIG_MEDIA_TYPE,
            "application/vnd.banlieue.microvm.config.v1+json"
        );
        assert_eq!(
            LAYER_KERNEL,
            "application/vnd.banlieue.microvm.kernel.v1+gzip"
        );
        assert_eq!(
            LAYER_INITRAMFS,
            "application/vnd.banlieue.microvm.initramfs.v1+gzip"
        );
        assert_eq!(LAYER_ROOTFS, "application/vnd.banlieue.microvm.rootfs.v1");
        assert_eq!(ANNOTATION_LAYER_SIZE, "io.banlieue.layer.size");
    }

    #[test]
    fn the_host_architecture_is_one_of_the_two() {
        let host = Architecture::host().unwrap();
        let expect = if cfg!(target_arch = "x86_64") {
            Architecture::Amd64
        } else {
            Architecture::Arm64
        };
        assert_eq!(host, expect);
    }

    // ---------------------------------------------------------------- config

    #[test]
    fn config_round_trips_with_the_adr_field_names() {
        let json =
            br#"{"schemaVersion":1,"architecture":"arm64","cmdline":"quiet","minMemoryMiB":128}"#;
        let c = MicrovmConfig::parse(json).unwrap();
        assert_eq!(c.architecture, Architecture::Arm64);
        assert_eq!(c.cmdline, "quiet");
        assert_eq!(c.min_memory_mib, 128);
        assert_eq!(MicrovmConfig::parse(&config_bytes(&c)).unwrap(), c);
    }

    #[test]
    fn config_with_an_unknown_field_is_refused() {
        let json = br#"{"schemaVersion":1,"architecture":"amd64","cmdline":"","minMemoryMiB":128,"extra":1}"#;
        let err = MicrovmConfig::parse(json).unwrap_err().to_string();
        assert!(err.contains("extra"), "{err}");
    }

    #[test]
    fn config_that_is_not_schema_one_is_refused() {
        let json = br#"{"schemaVersion":2,"architecture":"amd64","cmdline":"","minMemoryMiB":128}"#;
        let err = MicrovmConfig::parse(json).unwrap_err().to_string();
        assert!(err.contains("schema"), "{err}");
    }

    #[test]
    fn config_with_an_unknown_architecture_is_refused() {
        let json =
            br#"{"schemaVersion":1,"architecture":"riscv64","cmdline":"","minMemoryMiB":128}"#;
        assert!(MicrovmConfig::parse(json).is_err());
    }

    #[test]
    fn config_with_zero_minimum_memory_is_refused() {
        let json = br#"{"schemaVersion":1,"architecture":"amd64","cmdline":"","minMemoryMiB":0}"#;
        let err = MicrovmConfig::parse(json).unwrap_err().to_string();
        assert!(err.contains("minMemoryMiB"), "{err}");
    }

    #[test]
    fn config_whose_cmdline_sets_a_platform_argument_is_refused() {
        let json = br#"{"schemaVersion":1,"architecture":"amd64","cmdline":"quiet root=/dev/vdb","minMemoryMiB":128}"#;
        let err = MicrovmConfig::parse(json).unwrap_err().to_string();
        assert!(err.contains("root=/dev/vdb"), "{err}");
    }

    #[test]
    fn config_that_is_not_json_is_refused() {
        assert!(MicrovmConfig::parse(b"not json").is_err());
    }

    // ---------------------------------------------------------------- cmdline

    #[test]
    fn each_platform_argument_is_refused_and_named() {
        for bad in [
            "root=/dev/vdb",
            "ro",
            "rw",
            "ip=dhcp",
            "reboot=w",
            "panic=0",
            "pci=on",
            "console=hvc0",
        ] {
            let err = refuse_platform_args(&format!("quiet {bad} loglevel=3"))
                .unwrap_err()
                .to_string();
            assert!(err.contains(bad), "{bad}: {err}");
        }
    }

    #[test]
    fn a_platform_argument_is_refused_wherever_it_sits_and_whatever_separates_it() {
        assert!(refuse_platform_args("ro quiet").is_err());
        assert!(refuse_platform_args("quiet\tconsole=ttyS1").is_err());
        assert!(refuse_platform_args("a   b\nip=192.0.2.1::192.0.2.254:255.255.255.0").is_err());
    }

    #[test]
    fn lookalike_arguments_are_not_platform_arguments() {
        refuse_platform_args(
            "quiet loglevel=3 rootfstype=squashfs rootwait panic_on_oops=1 pcie_aspm=off \
             systemd.unit=sandbox.target ipv6.disable=1 rodata=on",
        )
        .unwrap();
        refuse_platform_args("").unwrap();
    }

    // ---------------------------------------------------------------- manifest

    fn layout() -> MicrovmLayout {
        MicrovmLayout {
            config: desc(CONFIG_MEDIA_TYPE, b"{}", &[]),
            kernel: gz_desc(LAYER_KERNEL, KERNEL),
            initramfs: Some(gz_desc(LAYER_INITRAMFS, INITRAMFS)),
            rootfs: desc(LAYER_ROOTFS, ROOTFS, &[]),
        }
    }

    #[test]
    fn a_built_manifest_has_the_artifact_type_and_layers_in_order() {
        let m = build_manifest(&layout());
        assert_eq!(m.artifact_type.as_deref(), Some(ARTIFACT_TYPE));
        assert_eq!(m.config.media_type, CONFIG_MEDIA_TYPE);
        let types: Vec<_> = m.layers.iter().map(|l| l.media_type.as_str()).collect();
        assert_eq!(types, [LAYER_KERNEL, LAYER_INITRAMFS, LAYER_ROOTFS]);
        assert_eq!(parse_manifest(&m).unwrap(), layout());
    }

    #[test]
    fn a_manifest_without_an_initramfs_parses() {
        let mut l = layout();
        l.initramfs = None;
        let m = build_manifest(&l);
        assert_eq!(m.layers.len(), 2);
        assert_eq!(parse_manifest(&m).unwrap(), l);
    }

    fn refused(m: &Manifest, needle: &str) {
        let err = parse_manifest(m).unwrap_err().to_string();
        assert!(err.contains(needle), "wanted {needle:?} in {err:?}");
    }

    #[test]
    fn a_manifest_of_another_artifact_type_is_refused() {
        let mut m = build_manifest(&layout());
        m.artifact_type = Some(ARTIFACT_TYPE_RAW.into());
        refused(&m, ARTIFACT_TYPE_RAW);
        m.artifact_type = None;
        refused(&m, "artifactType");
    }

    #[test]
    fn a_config_of_another_media_type_is_refused() {
        let mut m = build_manifest(&layout());
        m.config.media_type = "application/vnd.oci.empty.v1+json".into();
        refused(&m, "config");
    }

    #[test]
    fn an_unknown_layer_is_refused() {
        let mut m = build_manifest(&layout());
        m.layers.insert(
            1,
            desc(
                LAYER_MEDIA_TYPE_GZIP,
                b"x",
                &[(ANNOTATION_LAYER_SIZE, "1".into())],
            ),
        );
        refused(&m, LAYER_MEDIA_TYPE_GZIP);
    }

    #[test]
    fn a_misordered_layer_is_refused() {
        let mut m = build_manifest(&layout());
        m.layers.swap(0, 1);
        refused(&m, "order");
        let mut m = build_manifest(&layout());
        m.layers.swap(1, 2);
        refused(&m, "order");
    }

    #[test]
    fn a_missing_kernel_is_refused() {
        let mut m = build_manifest(&layout());
        m.layers.remove(0);
        refused(&m, "kernel");
    }

    #[test]
    fn a_missing_rootfs_is_refused() {
        let mut m = build_manifest(&layout());
        m.layers.pop();
        refused(&m, "rootfs");
    }

    #[test]
    fn a_repeated_or_trailing_layer_is_refused() {
        let mut m = build_manifest(&layout());
        m.layers.push(m.layers[2].clone());
        refused(&m, "order");
        let mut m = build_manifest(&layout());
        m.layers.insert(1, m.layers[0].clone());
        refused(&m, "order");
    }

    #[test]
    fn no_layers_at_all_is_refused() {
        let mut m = build_manifest(&layout());
        m.layers.clear();
        refused(&m, "kernel");
    }

    #[test]
    fn a_gzip_layer_without_its_size_annotation_is_refused() {
        let mut m = build_manifest(&layout());
        m.layers[0].annotations.clear();
        refused(&m, ANNOTATION_LAYER_SIZE);
        let mut m = build_manifest(&layout());
        m.layers[1]
            .annotations
            .insert(ANNOTATION_LAYER_SIZE.into(), "many".into());
        refused(&m, ANNOTATION_LAYER_SIZE);
    }

    #[test]
    fn a_layer_with_a_malformed_digest_is_refused() {
        let mut m = build_manifest(&layout());
        m.layers[2].digest = "md5:abc".into();
        refused(&m, "digest");
    }

    #[test]
    fn a_manifest_of_the_wrong_schema_or_media_type_is_refused() {
        let mut m = build_manifest(&layout());
        m.media_type = "application/vnd.oci.image.index.v1+json".into();
        refused(&m, "media type");
        let mut m = build_manifest(&layout());
        m.schema_version = 1;
        refused(&m, "schema");
        assert_eq!(MANIFEST_MEDIA_TYPE, build_manifest(&layout()).media_type);
    }

    // ---------------------------------------------------------------- push and pull

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn push_then_pull_lays_the_directory_out_as_the_adr_says() {
        let fake = Fake::start().await;
        let src = tempfile::tempdir().unwrap();
        let k = write(src.path(), "vmlinux", KERNEL);
        let i = write(src.path(), "initramfs.cpio", INITRAMFS);
        let r = write(src.path(), "rootfs.squashfs", ROOTFS);
        let scratch = tempfile::tempdir().unwrap();
        let cfg = config("quiet loglevel=3");

        let pushed = fake
            .client()
            .push_microvm(
                &fake.reference(),
                &MicrovmFiles {
                    kernel: &k,
                    initramfs: Some(&i),
                    rootfs: &r,
                },
                &cfg,
                scratch.path(),
            )
            .await
            .unwrap();
        assert!(entries(scratch.path()).is_empty(), "scratch is cleaned");
        let digest = pushed.reference.digest.clone().unwrap();

        let images = tempfile::tempdir().unwrap();
        let pulled = fake
            .client()
            .pull_microvm(
                &pushed.reference,
                images.path(),
                Architecture::Amd64,
                CEILING,
            )
            .await
            .unwrap();

        let name = digest.replace(':', "-");
        assert_eq!(entries(images.path()), std::slice::from_ref(&name));
        assert_eq!(pulled.dir, images.path().join(&name));
        assert_eq!(pulled.config, cfg);
        let d = &pulled.dir;
        assert_eq!(std::fs::read(d.join(KERNEL_FILE)).unwrap(), KERNEL);
        assert_eq!(std::fs::read(d.join(INITRAMFS_FILE)).unwrap(), INITRAMFS);
        assert_eq!(std::fs::read(d.join(ROOTFS_FILE)).unwrap(), ROOTFS);
        assert_eq!(
            std::fs::read(d.join(BOOT_FILE)).unwrap(),
            config_bytes(&cfg),
            "boot.json is the config blob verbatim"
        );
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & MODE_MASK;
        assert_eq!(mode(d), DIR_MODE);
        for f in [KERNEL_FILE, INITRAMFS_FILE, ROOTFS_FILE, BOOT_FILE] {
            assert_eq!(mode(&d.join(f)), FILE_MODE, "{f}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_rootfs_is_pushed_as_built_not_recompressed() {
        let fake = Fake::start().await;
        let src = tempfile::tempdir().unwrap();
        let k = write(src.path(), "k", KERNEL);
        let r = write(src.path(), "r", ROOTFS);
        let scratch = tempfile::tempdir().unwrap();
        fake.client()
            .push_microvm(
                &fake.reference(),
                &MicrovmFiles {
                    kernel: &k,
                    initramfs: None,
                    rootfs: &r,
                },
                &config(""),
                scratch.path(),
            )
            .await
            .unwrap();
        let st = fake.state.lock().unwrap();
        assert!(st.blobs.contains_key(&sha256_digest(ROOTFS)));
        assert!(st.blobs.contains_key(&sha256_digest(&gz(KERNEL))));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_image_without_an_initramfs_pulls_without_one() {
        let fake = Fake::start().await;
        let r = publish(&fake, &good_layout(&fake, &config(""), false));
        let images = tempfile::tempdir().unwrap();
        let pulled = fake
            .client()
            .pull_microvm(&r, images.path(), Architecture::Amd64, CEILING)
            .await
            .unwrap();
        assert!(pulled.dir.join(KERNEL_FILE).exists());
        assert!(!pulled.dir.join(INITRAMFS_FILE).exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn push_refuses_a_platform_argument_before_uploading_anything() {
        let fake = Fake::start().await;
        let src = tempfile::tempdir().unwrap();
        let k = write(src.path(), "k", KERNEL);
        let r = write(src.path(), "r", ROOTFS);
        let scratch = tempfile::tempdir().unwrap();
        let err = fake
            .client()
            .push_microvm(
                &fake.reference(),
                &MicrovmFiles {
                    kernel: &k,
                    initramfs: None,
                    rootfs: &r,
                },
                &config("quiet console=hvc0"),
                scratch.path(),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("console=hvc0"), "{err}");
        assert_eq!(fake.blob_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pulling_the_same_image_twice_is_idempotent() {
        let fake = Fake::start().await;
        let r = publish(&fake, &good_layout(&fake, &config(""), true));
        let images = tempfile::tempdir().unwrap();
        let first = fake
            .client()
            .pull_microvm(&r, images.path(), Architecture::Amd64, CEILING)
            .await
            .unwrap();
        let second = fake
            .client()
            .pull_microvm(&r, images.path(), Architecture::Amd64, CEILING)
            .await
            .unwrap();
        assert_eq!(first.dir, second.dir);
        assert_eq!(entries(images.path()).len(), 1);
    }

    // ---------------------------------------------------------------- Decision 6: fail closed

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_of_another_artifact_type() {
        let fake = Fake::start().await;
        let mut m = build_manifest(&good_layout(&fake, &config(""), true));
        m.artifact_type = Some(ARTIFACT_TYPE_RAW.into());
        let r = fake.put_manifest(&m);
        assert_refused_default(&fake, &r, ARTIFACT_TYPE_RAW).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_with_an_unknown_layer() {
        let fake = Fake::start().await;
        let mut m = build_manifest(&good_layout(&fake, &config(""), true));
        m.layers.insert(1, desc(LAYER_MEDIA_TYPE_GZIP, b"x", &[]));
        let r = fake.put_manifest(&m);
        assert_refused_default(&fake, &r, LAYER_MEDIA_TYPE_GZIP).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_with_a_misordered_layer() {
        let fake = Fake::start().await;
        let mut m = build_manifest(&good_layout(&fake, &config(""), true));
        m.layers.swap(0, 2);
        let r = fake.put_manifest(&m);
        assert_refused_default(&fake, &r, "order").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_with_a_missing_required_layer() {
        let fake = Fake::start().await;
        let mut m = build_manifest(&good_layout(&fake, &config(""), true));
        m.layers.pop();
        let r = fake.put_manifest(&m);
        assert_refused_default(&fake, &r, "rootfs").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_the_config_is_not_schema_one() {
        let fake = Fake::start().await;
        let mut l = good_layout(&fake, &config(""), true);
        let bad = br#"{"schemaVersion":2,"architecture":"amd64","cmdline":"","minMemoryMiB":128}"#;
        fake.put_blob(bad);
        l.config = desc(CONFIG_MEDIA_TYPE, bad, &[]);
        let r = publish(&fake, &l);
        assert_refused_default(&fake, &r, "schema").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_the_config_blob_does_not_hash_to_its_digest() {
        let fake = Fake::start().await;
        let mut l = good_layout(&fake, &config(""), true);
        let other = config_bytes(&config("quiet"));
        fake.put_blob_as(&l.config.digest, &other);
        l.config.size = other.len() as u64;
        let r = publish(&fake, &l);
        assert_refused_default(&fake, &r, "digest mismatch").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_the_image_cmdline_sets_a_platform_argument() {
        let fake = Fake::start().await;
        let r = publish(
            &fake,
            &good_layout(&fake, &config("quiet root=/dev/vdb"), true),
        );
        assert_refused_default(&fake, &r, "root=/dev/vdb").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_the_architecture_is_not_the_hosts() {
        let fake = Fake::start().await;
        let r = publish(&fake, &good_layout(&fake, &config(""), true));
        assert_refused(&fake, &r, Architecture::Arm64, CEILING, "architecture").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_a_gzip_layer_decompresses_to_more_than_annotated() {
        let fake = Fake::start().await;
        let mut l = good_layout(&fake, &config(""), true);
        l.kernel
            .annotations
            .insert(ANNOTATION_LAYER_SIZE.into(), (KERNEL.len() - 1).to_string());
        let r = publish(&fake, &l);
        assert_refused_default(&fake, &r, "kernel").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_a_gzip_layer_decompresses_to_less_than_annotated() {
        let fake = Fake::start().await;
        let mut l = good_layout(&fake, &config(""), true);
        let initramfs = l.initramfs.as_mut().unwrap();
        initramfs.annotations.insert(
            ANNOTATION_LAYER_SIZE.into(),
            (INITRAMFS.len() + 1).to_string(),
        );
        let r = publish(&fake, &l);
        assert_refused_default(&fake, &r, "initramfs").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_a_layer_exceeds_the_ceiling() {
        let fake = Fake::start().await;
        let r = publish(&fake, &good_layout(&fake, &config(""), true));
        // Every fixture layer is a few dozen bytes and the gzip streams are as long as the
        // rootfs, so a ceiling just under the rootfs trips on whichever layer is checked
        // first. Which layer it was is NOT what this test pins: the per-layer tests below do.
        let ceiling = ROOTFS.len() as u64 - 1;
        assert_refused(&fake, &r, Architecture::Amd64, ceiling, "ceiling").await;
    }

    /// Deterministic incompressible bytes: their gzip stream is longer than they are, so a
    /// ceiling equal to the plain length passes the annotated size and trips only on the
    /// compressed transfer.
    fn incompressible(len: usize) -> Vec<u8> {
        let mut seed: u32 = 1;
        (0..len)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 24) as u8
            })
            .collect()
    }

    /// The rootfs alone is over the ceiling. Every other layer is a few dozen bytes, so only
    /// the rootfs check can refuse this pull, and the error must name the rootfs.
    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_the_rootfs_alone_exceeds_the_ceiling() {
        let fake = Fake::start().await;
        let big = vec![0u8; 4 * KIB];
        fake.put_blob(&big);
        let mut l = good_layout(&fake, &config(""), true);
        l.rootfs = desc(LAYER_ROOTFS, &big, &[]);
        let r = publish(&fake, &l);
        let ceiling = 2 * KIB as u64;
        assert_refused(&fake, &r, Architecture::Amd64, ceiling, "rootfs is").await;
    }

    /// The compressed initramfs alone is over the ceiling: the kernel and rootfs fit, so only
    /// the initramfs check can refuse this pull, and the error must name the initramfs.
    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_the_compressed_initramfs_exceeds_the_ceiling() {
        let fake = Fake::start().await;
        let plain = incompressible(4 * KIB);
        let compressed = gz(&plain);
        assert!(compressed.len() > plain.len());
        fake.put_blob(&compressed);
        let mut l = good_layout(&fake, &config(""), true);
        l.initramfs = Some(desc(
            LAYER_INITRAMFS,
            &compressed,
            &[(ANNOTATION_LAYER_SIZE, plain.len().to_string())],
        ));
        let r = publish(&fake, &l);
        let ceiling = plain.len() as u64;
        assert_refused(&fake, &r, Architecture::Amd64, ceiling, "initramfs is").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_the_annotated_size_exceeds_the_ceiling() {
        let fake = Fake::start().await;
        let mut l = good_layout(&fake, &config(""), true);
        l.kernel
            .annotations
            .insert(ANNOTATION_LAYER_SIZE.into(), CEILING.to_string());
        let r = publish(&fake, &l);
        assert_refused(&fake, &r, Architecture::Amd64, CEILING - 1, "ceiling").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_the_compressed_layer_exceeds_the_ceiling() {
        let fake = Fake::start().await;
        let plain = incompressible(4 * KIB);
        let compressed = gz(&plain);
        assert!(compressed.len() > plain.len());
        fake.put_blob(&compressed);
        let mut l = good_layout(&fake, &config(""), true);
        l.kernel = desc(
            LAYER_KERNEL,
            &compressed,
            &[(ANNOTATION_LAYER_SIZE, plain.len().to_string())],
        );
        let r = publish(&fake, &l);
        let ceiling = plain.len() as u64;
        assert_refused(&fake, &r, Architecture::Amd64, ceiling, "kernel is").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_a_gzip_layer_does_not_hash_to_its_digest() {
        let fake = Fake::start().await;
        let l = good_layout(&fake, &config(""), true);
        // Same length, different bytes: only the digest can tell.
        let mut forged = gz(KERNEL);
        let last = forged.len() - 1;
        forged[last] ^= 0xff;
        fake.put_blob_as(&l.kernel.digest, &forged);
        let r = publish(&fake, &l);
        assert_refused_default(&fake, &r, "digest mismatch").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_the_rootfs_does_not_hash_to_its_digest() {
        let fake = Fake::start().await;
        let l = good_layout(&fake, &config(""), true);
        let mut forged = ROOTFS.to_vec();
        forged[0] ^= 0xff;
        fake.put_blob_as(&l.rootfs.digest, &forged);
        let r = publish(&fake, &l);
        assert_refused_default(&fake, &r, "digest mismatch").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_the_rootfs_is_longer_than_its_descriptor() {
        let fake = Fake::start().await;
        let mut l = good_layout(&fake, &config(""), true);
        l.rootfs.size -= 1;
        let r = publish(&fake, &l);
        assert_refused_default(&fake, &r, "rootfs").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_the_rootfs_is_shorter_than_its_descriptor() {
        let fake = Fake::start().await;
        let mut l = good_layout(&fake, &config(""), true);
        l.rootfs.size += 1;
        let r = publish(&fake, &l);
        assert_refused_default(&fake, &r, "rootfs").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_pull_when_a_blob_is_missing_from_the_registry() {
        let fake = Fake::start().await;
        let l = good_layout(&fake, &config(""), true);
        fake.state.lock().unwrap().blobs.remove(&l.rootfs.digest);
        let r = publish(&fake, &l);
        assert_refused_default(&fake, &r, "404").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pull_by_tag_is_refused() {
        let fake = Fake::start().await;
        let images = tempfile::tempdir().unwrap();
        let err = fake
            .client()
            .pull_microvm(
                &Reference::parse(&format!("{}/sandbox/image:latest", fake.addr)).unwrap(),
                images.path(),
                Architecture::Amd64,
                CEILING,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("digest"), "{err}");
        assert!(entries(images.path()).is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_pull_does_not_disturb_an_image_already_in_place() {
        let fake = Fake::start().await;
        let good = publish(&fake, &good_layout(&fake, &config(""), true));
        let images = tempfile::tempdir().unwrap();
        let pulled = fake
            .client()
            .pull_microvm(&good, images.path(), Architecture::Amd64, CEILING)
            .await
            .unwrap();

        let mut l = good_layout(&fake, &config("other"), true);
        l.rootfs.size += 1;
        let bad = publish(&fake, &l);
        fake.client()
            .pull_microvm(&bad, images.path(), Architecture::Amd64, CEILING)
            .await
            .unwrap_err();
        assert_eq!(
            entries(images.path()),
            [pulled
                .dir
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()]
        );
        assert_eq!(std::fs::read(pulled.dir.join(ROOTFS_FILE)).unwrap(), ROOTFS);
    }
}
