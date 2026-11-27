// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `sigv4.rs`, including AWS's published test vectors
//! (`tests/fixtures/sigv4/README.md` names their sources).

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use jiff::Timestamp;

    use super::super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sigv4");

    /// Every aws-c-auth case kept in the fixtures.
    const AWS_C_AUTH_CASES: [&str; 36] = [
        "get-header-key-duplicate",
        "get-header-value-order",
        "get-header-value-trim",
        "get-relative-normalized",
        "get-relative-relative-normalized",
        "get-relative-relative-unnormalized",
        "get-relative-unnormalized",
        "get-slash-dot-slash-normalized",
        "get-slash-dot-slash-unnormalized",
        "get-slash-normalized",
        "get-slash-pointless-dot-normalized",
        "get-slash-pointless-dot-unnormalized",
        "get-slash-unnormalized",
        "get-slashes-normalized",
        "get-slashes-unnormalized",
        "get-space-normalized",
        "get-space-unnormalized",
        "get-unreserved",
        "get-utf8",
        "get-vanilla",
        "get-vanilla-empty-query-key",
        "get-vanilla-query",
        "get-vanilla-query-order-encoded",
        "get-vanilla-query-order-key-case",
        "get-vanilla-query-unreserved",
        "get-vanilla-utf8-query",
        "get-vanilla-with-session-token",
        "post-header-key-case",
        "post-header-key-sort",
        "post-header-value-case",
        "post-sts-header-before",
        "post-vanilla",
        "post-vanilla-empty-query-value",
        "post-vanilla-query",
        "post-x-www-form-urlencoded",
        "post-x-www-form-urlencoded-parameters",
    ];

    /// The S3 API Reference's worked examples.
    const S3_DOC_CASES: [&str; 4] = [
        "get-object",
        "put-object",
        "get-bucket-lifecycle",
        "list-objects",
    ];

    const HTTP_VERSION_SUFFIX: &str = " HTTP/1.1";
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const TEST_ACCESS_KEY: &str = "AKIDEXAMPLE";
    const TEST_SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
    const TEST_TOKEN: &str = "session-token-value";
    const TEST_HOST: &str = "ec2.us-east-1.amazonaws.com";
    const TEST_TIME: &str = "2015-08-30T12:36:00Z";

    /// One test case, read into this module's request description.
    struct Fixture {
        dir: PathBuf,
        method: String,
        host: String,
        path: String,
        query: Vec<(String, String)>,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        region: String,
        service: String,
        timestamp: Timestamp,
        credentials: Credentials,
        options: SigningOptions,
    }

    impl Fixture {
        fn request(&self) -> SigningRequest<'_> {
            SigningRequest {
                method: &self.method,
                host: &self.host,
                path: &self.path,
                query: &self.query,
                headers: &self.headers,
                payload: Payload::Bytes(&self.body),
                region: &self.region,
                service: &self.service,
                timestamp: self.timestamp,
            }
        }

        fn expected(&self, file: &str) -> String {
            std::fs::read_to_string(self.dir.join(file))
                .unwrap_or_else(|e| panic!("{}/{file}: {e}", self.dir.display()))
        }
    }

    /// Decodes `%XX` escapes (the suite's query strings carry some).
    fn percent_decode(input: &str) -> String {
        let bytes = input.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).expect("ascii");
                out.push(u8::from_str_radix(hex, 16).expect("hex escape"));
                i += 3;
                continue;
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8(out).expect("utf-8")
    }

    fn load(dir: &Path) -> Fixture {
        let context: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("context.json")).expect("context.json"),
        )
        .expect("context.json is JSON");
        let creds = &context["credentials"];
        let credentials = Credentials {
            access_key_id: creds["access_key_id"].as_str().expect("akid").to_string(),
            secret_access_key: creds["secret_access_key"]
                .as_str()
                .expect("secret")
                .to_string(),
            session_token: creds["token"].as_str().map(str::to_string),
        };
        let options = SigningOptions {
            normalize_path: context["normalize"].as_bool().expect("normalize"),
            // The suite's paths are the wire form, unencoded: see the README.
            double_encode_path: false,
            content_sha256_header: context["sign_body"].as_bool().expect("sign_body"),
        };

        let raw = std::fs::read_to_string(dir.join("request.txt")).expect("request.txt");
        let (head, body) = raw.split_once("\n\n").unwrap_or((raw.as_str(), ""));
        let mut lines = head.lines();
        let request_line = lines
            .next()
            .expect("request line")
            .strip_suffix(HTTP_VERSION_SUFFIX)
            .expect("HTTP/1.1");
        let (method, target) = request_line.split_once(' ').expect("method");
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let query = query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                (percent_decode(k), percent_decode(v))
            })
            .collect();
        let mut host = String::new();
        let mut headers = Vec::new();
        for line in lines.filter(|l| !l.is_empty()) {
            let (name, value) = line.split_once(':').expect("header");
            if name.eq_ignore_ascii_case("host") {
                host = value.trim().to_string();
                continue;
            }
            headers.push((name.to_string(), value.to_string()));
        }
        Fixture {
            dir: dir.to_path_buf(),
            method: method.to_string(),
            host,
            path: path.to_string(),
            query,
            headers,
            body: body.as_bytes().to_vec(),
            region: context["region"].as_str().expect("region").to_string(),
            service: context["service"].as_str().expect("service").to_string(),
            timestamp: context["timestamp"]
                .as_str()
                .expect("timestamp")
                .parse()
                .expect("RFC 3339 timestamp"),
            credentials,
            options,
        }
    }

    fn assert_vector(dir: &Path) {
        let fixture = load(dir);
        let name = dir.display();
        let signed = sign(&fixture.request(), &fixture.credentials, &fixture.options)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            signed.canonical_request,
            fixture.expected("header-canonical-request.txt"),
            "{name}: canonical request"
        );
        assert_eq!(
            signed.string_to_sign,
            fixture.expected("header-string-to-sign.txt"),
            "{name}: string to sign"
        );
        assert_eq!(
            signed.signature,
            fixture.expected("header-signature.txt").trim_end(),
            "{name}: signature"
        );
        let signed_request = dir.join("header-signed-request.txt");
        if !signed_request.exists() {
            return;
        }
        let text = std::fs::read_to_string(signed_request).expect("signed request");
        let expected = text
            .lines()
            .find_map(|l| l.strip_prefix("Authorization:"))
            .expect("Authorization line");
        let actual = signed
            .headers
            .iter()
            .find(|(k, _)| k == AUTHORIZATION)
            .map(|(_, v)| v.as_str());
        assert_eq!(actual, Some(expected), "{name}: Authorization header");
    }

    fn test_request<'a>(
        query: &'a [(String, String)],
        headers: &'a [(String, String)],
        payload: Payload<'a>,
    ) -> SigningRequest<'a> {
        SigningRequest {
            method: "POST",
            host: TEST_HOST,
            path: "/",
            query,
            headers,
            payload,
            region: "us-east-1",
            service: "ec2",
            timestamp: TEST_TIME.parse().expect("timestamp"),
        }
    }

    fn test_credentials(token: Option<&str>) -> Credentials {
        Credentials::new(TEST_ACCESS_KEY, TEST_SECRET, token.map(str::to_string))
    }

    fn header<'a>(signed: &'a Signed, name: &str) -> Option<&'a str> {
        signed
            .headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    // --- AWS's vectors ------------------------------------------------------

    #[test]
    fn every_aws_c_auth_vector_matches_byte_for_byte() {
        for case in AWS_C_AUTH_CASES {
            assert_vector(&Path::new(FIXTURES).join("aws-c-auth").join(case));
        }
    }

    #[test]
    fn every_s3_documentation_example_matches_byte_for_byte() {
        for case in S3_DOC_CASES {
            assert_vector(&Path::new(FIXTURES).join("s3-docs").join(case));
        }
    }

    #[test]
    fn the_s3_documentation_examples_sign_in_s3_style() {
        for case in S3_DOC_CASES {
            let mut fixture = load(&Path::new(FIXTURES).join("s3-docs").join(case));
            fixture.options = SigningOptions::s3();
            let signed =
                sign(&fixture.request(), &fixture.credentials, &fixture.options).expect("signs");
            assert_eq!(
                signed.signature,
                fixture.expected("header-signature.txt").trim_end(),
                "{case}"
            );
        }
    }

    #[test]
    fn the_fixture_list_covers_every_case_on_disk() {
        let on_disk = std::fs::read_dir(Path::new(FIXTURES).join("aws-c-auth"))
            .expect("fixtures")
            .count();
        assert_eq!(on_disk, AWS_C_AUTH_CASES.len());
    }

    #[test]
    fn the_signing_key_is_derived_by_the_documented_hmac_chain() {
        let ts: Timestamp = TEST_TIME.parse().expect("timestamp");
        let key = signing_key(TEST_SECRET, ts, "us-east-1", "service");
        let date_key = hmac_sha256(b"AWS4wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", b"20150830");
        let region_key = hmac_sha256(&date_key, b"us-east-1");
        let service_key = hmac_sha256(&region_key, b"service");
        assert_eq!(key, hmac_sha256(&service_key, b"aws4_request"));
        // And it produces get-vanilla's signature from its string to sign.
        let sts = std::fs::read_to_string(
            Path::new(FIXTURES).join("aws-c-auth/get-vanilla/header-string-to-sign.txt"),
        )
        .expect("fixture");
        assert_eq!(
            signature(&key, &sts),
            "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    // --- Credentials --------------------------------------------------------

    #[test]
    fn debug_redacts_the_secret_and_the_session_token() {
        let creds = test_credentials(Some(TEST_TOKEN));
        let shown = format!("{creds:?}");
        assert!(shown.contains(TEST_ACCESS_KEY), "{shown}");
        assert!(!shown.contains(TEST_SECRET), "{shown}");
        assert!(!shown.contains(TEST_TOKEN), "{shown}");
        assert!(shown.contains("redacted"), "{shown}");
    }

    #[test]
    fn debug_of_a_signed_request_does_not_show_the_session_token() {
        let creds = test_credentials(Some(TEST_TOKEN));
        let signed = sign(
            &test_request(&[], &[], Payload::Bytes(b"")),
            &creds,
            &SigningOptions::standard(),
        )
        .expect("signs");
        let shown = format!("{signed:?}");
        assert!(!shown.contains(TEST_TOKEN), "{shown}");
        assert!(shown.contains(&signed.signature), "{shown}");
    }

    // --- Headers ------------------------------------------------------------

    #[test]
    fn a_session_token_adds_a_signed_security_token_header() {
        let signed = sign(
            &test_request(&[], &[], Payload::Bytes(b"")),
            &test_credentials(Some(TEST_TOKEN)),
            &SigningOptions::standard(),
        )
        .expect("signs");
        assert_eq!(header(&signed, X_AMZ_SECURITY_TOKEN), Some(TEST_TOKEN));
        assert!(
            signed
                .canonical_request
                .contains(&format!("\nx-amz-security-token:{TEST_TOKEN}\n"))
        );
        let auth = header(&signed, AUTHORIZATION).expect("Authorization");
        assert!(
            auth.contains("SignedHeaders=host;x-amz-date;x-amz-security-token,"),
            "{auth}"
        );
    }

    #[test]
    fn without_a_session_token_no_security_token_header_is_sent() {
        let signed = sign(
            &test_request(&[], &[], Payload::Bytes(b"")),
            &test_credentials(None),
            &SigningOptions::standard(),
        )
        .expect("signs");
        assert_eq!(header(&signed, X_AMZ_SECURITY_TOKEN), None);
        assert!(!signed.canonical_request.contains("x-amz-security-token"));
    }

    #[test]
    fn the_date_header_is_the_basic_iso8601_timestamp() {
        let signed = sign(
            &test_request(&[], &[], Payload::Bytes(b"")),
            &test_credentials(None),
            &SigningOptions::standard(),
        )
        .expect("signs");
        assert_eq!(header(&signed, X_AMZ_DATE), Some("20150830T123600Z"));
        assert_eq!(
            amz_date(TEST_TIME.parse().expect("timestamp")),
            "20150830T123600Z"
        );
        assert_eq!(
            credential_scope(TEST_TIME.parse().expect("timestamp"), "eu-west-1", "ec2"),
            "20150830/eu-west-1/ec2/aws4_request"
        );
    }

    #[test]
    fn the_content_sha256_header_is_sent_only_when_asked_for() {
        let creds = test_credentials(None);
        let request = test_request(&[], &[], Payload::Bytes(b""));
        let plain = sign(&request, &creds, &SigningOptions::standard()).expect("signs");
        assert_eq!(header(&plain, X_AMZ_CONTENT_SHA256), None);
        let s3 = sign(&request, &creds, &SigningOptions::s3()).expect("signs");
        assert_eq!(header(&s3, X_AMZ_CONTENT_SHA256), Some(EMPTY_SHA256));
        assert!(
            s3.canonical_request
                .contains("x-amz-content-sha256;x-amz-date")
        );
    }

    #[test]
    fn a_caller_header_this_module_sets_is_refused() {
        let creds = test_credentials(None);
        for name in [
            "Host",
            "X-Amz-Date",
            "authorization",
            "X-Amz-Security-Token",
            "x-amz-content-sha256",
        ] {
            let headers = vec![(name.to_string(), "x".to_string())];
            let result = sign(
                &test_request(&[], &headers, Payload::Bytes(b"")),
                &creds,
                &SigningOptions::standard(),
            );
            assert!(matches!(result, Err(Error::Invalid(_))), "{name}");
        }
    }

    #[test]
    fn a_header_that_could_split_the_request_is_refused() {
        let creds = test_credentials(None);
        for (name, value) in [
            ("X-Bad", "a\r\nInjected: 1"),
            ("X-Bad", "a\nb"),
            ("X Bad", "a"),
            ("X:Bad", "a"),
            ("", "a"),
        ] {
            let headers = vec![(name.to_string(), value.to_string())];
            let result = sign(
                &test_request(&[], &headers, Payload::Bytes(b"")),
                &creds,
                &SigningOptions::standard(),
            );
            assert!(
                matches!(result, Err(Error::Invalid(_))),
                "{name:?}: {value:?}"
            );
        }
    }

    #[test]
    fn caller_headers_are_lowercased_trimmed_and_sorted() {
        let headers = vec![
            ("X-Zeta".to_string(), "  last  ".to_string()),
            ("Content-Type".to_string(), "a   b\tc".to_string()),
        ];
        let canonical = canonical_request(
            &test_request(&[], &headers, Payload::Bytes(b"")),
            &test_credentials(None),
            &SigningOptions::standard(),
        )
        .expect("canonical");
        assert!(
            canonical.contains(
                "content-type:a b c\nhost:ec2.us-east-1.amazonaws.com\nx-amz-date:20150830T123600Z\nx-zeta:last\n\ncontent-type;host;x-amz-date;x-zeta\n"
            ),
            "{canonical}"
        );
    }

    // --- Payload ------------------------------------------------------------

    #[test]
    fn the_hash_of_an_empty_body_is_the_sha256_of_nothing() {
        assert_eq!(payload_hash(Payload::Bytes(b"")), EMPTY_SHA256);
        assert_eq!(sha256_hex(b""), EMPTY_SHA256);
        let canonical = canonical_request(
            &test_request(&[], &[], Payload::Bytes(b"")),
            &test_credentials(None),
            &SigningOptions::standard(),
        )
        .expect("canonical");
        assert!(
            canonical.ends_with(&format!("\n{EMPTY_SHA256}")),
            "{canonical}"
        );
    }

    #[test]
    fn unsigned_payload_is_used_verbatim() {
        assert_eq!(payload_hash(Payload::Unsigned), UNSIGNED_PAYLOAD);
        let signed = sign(
            &test_request(&[], &[], Payload::Unsigned),
            &test_credentials(None),
            &SigningOptions::s3(),
        )
        .expect("signs");
        assert!(
            signed.canonical_request.ends_with("\nUNSIGNED-PAYLOAD"),
            "{}",
            signed.canonical_request
        );
        assert_eq!(
            header(&signed, X_AMZ_CONTENT_SHA256),
            Some(UNSIGNED_PAYLOAD)
        );
    }

    #[test]
    fn payload_debug_shows_the_length_not_the_bytes() {
        let shown = format!("{:?}", Payload::Bytes(b"secret-body"));
        assert!(!shown.contains("secret"), "{shown}");
        assert!(shown.contains("11"), "{shown}");
    }

    // --- Encoding -----------------------------------------------------------

    #[test]
    fn uri_encode_keeps_only_unreserved_bytes() {
        assert_eq!(uri_encode("AZaz09-_.~", true), "AZaz09-_.~");
        assert_eq!(uri_encode("a b", true), "a%20b");
        assert_eq!(uri_encode("a+b=c&d", true), "a%2Bb%3Dc%26d");
        assert_eq!(uri_encode("a/b", true), "a%2Fb");
        assert_eq!(uri_encode("a/b", false), "a/b");
        assert_eq!(uri_encode("\u{1234}", true), "%E1%88%B4");
        assert_eq!(uri_encode("*", true), "%2A");
    }

    #[test]
    fn the_canonical_query_is_sorted_by_encoded_key_then_value() {
        let query = vec![
            ("b".to_string(), "2".to_string()),
            ("a".to_string(), "z".to_string()),
            ("a".to_string(), "y".to_string()),
            ("a b".to_string(), "c d".to_string()),
        ];
        assert_eq!(canonical_query(&query), "a=y&a=z&a%20b=c%20d&b=2");
        assert_eq!(canonical_query(&[]), "");
    }

    #[test]
    fn standard_paths_are_normalized_and_encoded_twice() {
        let standard = SigningOptions::standard();
        assert_eq!(canonical_uri("", &standard), "/");
        assert_eq!(canonical_uri("/", &standard), "/");
        assert_eq!(canonical_uri("//a/./b/../c//", &standard), "/a/c/");
        assert_eq!(canonical_uri("/a/..", &standard), "/");
        assert_eq!(canonical_uri("/a b", &standard), "/a%2520b");
    }

    #[test]
    fn s3_paths_are_encoded_once_and_never_normalized() {
        let s3 = SigningOptions::s3();
        assert_eq!(canonical_uri("/bucket/a b/../c", &s3), "/bucket/a%20b/../c");
        assert_eq!(canonical_uri("//key", &s3), "//key");
        assert_eq!(canonical_uri("", &s3), "/");
    }

    #[test]
    fn the_wire_path_is_the_path_encoded_once() {
        assert_eq!(
            wire_path("/bucket/a b+c", &SigningOptions::s3()),
            "/bucket/a%20b%2Bc"
        );
        assert_eq!(wire_path("//a/./b", &SigningOptions::standard()), "/a/b");
        assert_eq!(wire_path("", &SigningOptions::standard()), "/");
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex_lower(&[0x00, 0xab, 0x0f, 0xff]), "00ab0fff");
    }
}
