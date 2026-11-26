// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `upload.rs`: the SAS host check, Put Page against a local
//! server (no bearer token, the right headers), and the uploader against
//! the fake page blob (zero ranges skipped, retries, short sources).

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::super::*;
    use crate::fake_blob::FakePageBlob;
    use crate::test_server::{install_provider, respond, serve};

    const SUFFIX: &str = "blob.storage.azure.net";
    const CHUNK: usize = 4096;

    fn sas(url: &str) -> SasUrl {
        SasUrl::new(url.to_string())
    }

    fn fast() -> UploadOptions {
        UploadOptions {
            chunk_bytes: CHUNK,
            parallelism: 3,
            max_attempts: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(2),
        }
    }

    #[test]
    fn sas_never_prints() {
        let s = sas("https://md-x.z1.blob.storage.azure.net/d/abcd?sv=1&sig=SECRET");
        assert!(!format!("{s:?}").contains("SECRET"));
        assert!(!format!("{s}").contains("SECRET"));
        assert_eq!(s.host().as_deref(), Some("md-x.z1.blob.storage.azure.net"));
    }

    #[test]
    fn the_upload_host_is_checked() {
        let ok = "https://md-x.z1.blob.storage.azure.net/d/abcd?sv=1&sig=S";
        assert!(validate_upload_url(&sas(ok), SUFFIX).is_ok());
        for bad in [
            "https://md-x.blob.storage.azure.net.evil.example/d?sig=S",
            "https://evilblob.storage.azure.net/d?sig=S",
            "https://md-x.blob.core.windows.net/d?sig=S",
            "http://md-x.z1.blob.storage.azure.net/d?sig=S",
            "https://user@md-x.z1.blob.storage.azure.net/d?sig=S",
            "https://md-x.z1.blob.storage.azure.net:8443/d?sig=S",
            "https://md-x.z1.blob.storage.azure.net/d",
            "not a url",
        ] {
            let err = validate_upload_url(&sas(bad), SUFFIX).unwrap_err();
            assert!(matches!(err, crate::Error::UntrustedUploadHost(_)), "{bad}");
            assert!(!err.to_string().contains("sig=S"), "{bad}: {err}");
        }
    }

    #[test]
    fn loopback_http_is_only_for_a_loopback_suffix() {
        let local = "http://127.0.0.1:1234/d?sig=S";
        assert!(validate_upload_url(&sas(local), "127.0.0.1").is_ok());
        assert!(validate_upload_url(&sas(local), SUFFIX).is_err());
    }

    #[test]
    fn page_ranges_must_be_whole_pages_up_to_4_mib() {
        assert!(check_page_range(0, 512).is_ok());
        assert!(check_page_range(512, MAX_PAGE_WRITE_BYTES).is_ok());
        assert!(check_page_range(1, 512).is_err());
        assert!(check_page_range(0, 511).is_err());
        assert!(check_page_range(0, 0).is_err());
        assert!(check_page_range(0, MAX_PAGE_WRITE_BYTES + 512).is_err());
    }

    #[test]
    fn the_http_factory_refuses_a_foreign_host_before_any_request() {
        let factory = HttpPageWriterFactory::new(SUFFIX);
        let err = factory
            .writer(&sas("https://attacker.example/d?sig=S"))
            .err()
            .expect("refused");
        assert!(matches!(err, crate::Error::UntrustedUploadHost(_)));
    }

    #[tokio::test]
    async fn put_page_sends_the_range_headers_and_no_bearer_token() {
        install_provider();
        let (base, server) = serve(vec![respond(201, &[], "")]).await;
        let url = format!("{base}/rg/disk/abcd?sv=2018-03-28&sr=b&sig=SECRET");
        let factory = HttpPageWriterFactory::new("127.0.0.1");
        let writer = factory.writer(&sas(&url)).expect("loopback accepted");
        writer.put_page(1024, vec![7u8; 1024]).await.expect("201");
        let seen = server.await.unwrap();
        assert_eq!(seen.len(), 1);
        let r = &seen[0];
        assert!(
            r.target().starts_with("PUT /rg/disk/abcd?"),
            "{}",
            r.target()
        );
        assert!(r.target().ends_with("&comp=page"), "{}", r.target());
        assert_eq!(r.header("x-ms-page-write").as_deref(), Some("update"));
        assert_eq!(r.header("x-ms-range").as_deref(), Some("bytes=1024-2047"));
        assert_eq!(r.header("x-ms-version").as_deref(), Some(BLOB_API_VERSION));
        assert_eq!(r.header("content-length").as_deref(), Some("1024"));
        assert!(
            r.header("authorization").is_none(),
            "no ARM token on a SAS request"
        );
        assert_eq!(r.body, vec![7u8; 1024]);
    }

    #[tokio::test]
    async fn a_busy_blob_service_is_retryable_and_the_error_hides_the_sas() {
        install_provider();
        let (base, server) = serve(vec![respond(503, &[], "")]).await;
        let url = format!("{base}/rg/disk/abcd?sig=SECRET");
        let writer = HttpPageWriterFactory::new("127.0.0.1")
            .writer(&sas(&url))
            .unwrap();
        let err = writer.put_page(0, vec![1u8; 512]).await.unwrap_err();
        assert!(err.is_retryable());
        assert!(!err.to_string().contains("SECRET"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_transport_error_hides_the_sas() {
        install_provider();
        // Nothing listens here once the listener is dropped.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let url = format!("http://{addr}/rg/disk?sig=SECRET");
        let writer = HttpPageWriterFactory::new("127.0.0.1")
            .writer(&sas(&url))
            .unwrap();
        let err = writer.put_page(0, vec![1u8; 512]).await.unwrap_err();
        assert!(matches!(err, crate::Error::Transport(_)));
        assert!(!err.to_string().contains("SECRET"), "{err}");
    }

    /// Non-zero chunks at 0 and 3, zero chunks at 1, 2 and 4.
    fn sample() -> Vec<u8> {
        let mut data = vec![0u8; CHUNK * 5];
        data[10] = 1;
        data[CHUNK * 3 + 100] = 9;
        data
    }

    #[tokio::test]
    async fn zero_ranges_are_skipped_and_the_rest_written_exactly() {
        let data = sample();
        let blob = Arc::new(FakePageBlob::new(data.len() as u64));
        let stats = upload_pages(&data[..], data.len() as u64, blob.clone(), &fast())
            .await
            .unwrap();
        assert_eq!(stats.ranges_written, 2);
        assert_eq!(stats.bytes_written, 2 * CHUNK as u64);
        assert_eq!(stats.bytes_skipped, 3 * CHUNK as u64);
        assert_eq!(stats.bytes_total, data.len() as u64);
        blob.check_nonzero_written(&data, CHUNK).unwrap();
    }

    #[tokio::test]
    async fn an_all_zero_disk_writes_nothing() {
        let data = vec![0u8; CHUNK * 3];
        let blob = Arc::new(FakePageBlob::new(data.len() as u64));
        let stats = upload_pages(&data[..], data.len() as u64, blob.clone(), &fast())
            .await
            .unwrap();
        assert_eq!(stats.ranges_written, 0);
        assert_eq!(blob.writes(), 0);
    }

    #[tokio::test]
    async fn the_last_range_may_be_shorter_than_a_chunk() {
        let mut data = vec![3u8; CHUNK + 512];
        data[..CHUNK].fill(0);
        let blob = Arc::new(FakePageBlob::new(data.len() as u64));
        upload_pages(&data[..], data.len() as u64, blob.clone(), &fast())
            .await
            .unwrap();
        assert_eq!(blob.written_ranges(), vec![(CHUNK as u64, 512)]);
    }

    #[tokio::test]
    async fn busy_writes_are_retried() {
        let data = sample();
        let blob = Arc::new(FakePageBlob::new(data.len() as u64));
        blob.fail_next_writes(2);
        upload_pages(&data[..], data.len() as u64, blob.clone(), &fast())
            .await
            .unwrap();
        blob.check_nonzero_written(&data, CHUNK).unwrap();
    }

    #[tokio::test]
    async fn a_write_that_keeps_failing_fails_the_upload() {
        let data = sample();
        let blob = Arc::new(FakePageBlob::new(data.len() as u64));
        blob.fail_next_writes(100);
        let err = upload_pages(&data[..], data.len() as u64, blob, &fast())
            .await
            .unwrap_err();
        assert!(matches!(err, crate::Error::Server { status: 503, .. }));
    }

    #[tokio::test]
    async fn a_short_or_long_source_is_refused() {
        let data = sample();
        let blob = Arc::new(FakePageBlob::new(data.len() as u64 + CHUNK as u64));
        let err = upload_pages(&data[..], data.len() as u64 + CHUNK as u64, blob, &fast())
            .await
            .unwrap_err();
        assert!(matches!(err, crate::Error::UploadSource(_)));

        let blob = Arc::new(FakePageBlob::new(data.len() as u64));
        let err = upload_pages(&data[..], data.len() as u64 - 512, blob, &fast())
            .await
            .unwrap_err();
        assert!(matches!(err, crate::Error::UploadSource(_)));
    }

    #[tokio::test]
    async fn options_that_break_page_alignment_are_refused() {
        let data = sample();
        let blob = Arc::new(FakePageBlob::new(data.len() as u64));
        let mut options = fast();
        options.chunk_bytes = 1000;
        assert!(
            upload_pages(&data[..], data.len() as u64, blob.clone(), &options)
                .await
                .is_err()
        );
        assert!(
            upload_pages(&data[..100], 100, blob, &fast())
                .await
                .is_err()
        );
    }

    #[test]
    fn defaults_are_the_named_limits() {
        let d = UploadOptions::default();
        assert_eq!(d.chunk_bytes, MAX_PAGE_WRITE_BYTES);
        assert_eq!(d.parallelism, PARALLEL_PAGE_WRITES);
        assert_eq!(d.max_attempts, MAX_WRITE_ATTEMPTS);
    }
}
