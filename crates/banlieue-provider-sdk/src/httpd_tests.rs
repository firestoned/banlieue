// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for [`super::super::httpd`]: request-line parsing and response
//! serialisation, as pure functions of bytes.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn parses_a_kubelet_probe() {
        let line = parse_request_line(b"GET /readyz HTTP/1.1\r\nHost: 192.0.2.10:8081\r\n\r\n")
            .expect("valid request line");
        assert_eq!(
            line,
            RequestLine {
                method: "GET",
                path: "/readyz"
            }
        );
    }

    #[test]
    fn accepts_a_bare_newline_terminator() {
        let line = parse_request_line(b"GET /livez HTTP/1.0\n").expect("valid request line");
        assert_eq!(line.path, "/livez");
    }

    #[test]
    fn strips_the_query_string_from_the_path() {
        let line = parse_request_line(b"GET /metrics?name[]=x HTTP/1.1\r\n").expect("valid");
        assert_eq!(line.path, "/metrics");
    }

    #[test]
    fn rejects_an_unterminated_short_line_as_malformed() {
        assert_eq!(
            parse_request_line(b"GET /livez HTTP/1.1"),
            Err(RequestError::Malformed)
        );
    }

    #[test]
    fn rejects_empty_input_as_malformed() {
        assert_eq!(parse_request_line(b""), Err(RequestError::Malformed));
    }

    #[test]
    fn rejects_an_oversized_line_without_terminator() {
        let buf = vec![b'A'; MAX_REQUEST_LINE_BYTES];
        assert_eq!(parse_request_line(&buf), Err(RequestError::TooLarge));
    }

    #[test]
    fn rejects_a_terminator_past_the_cap() {
        let mut buf = b"GET /".to_vec();
        buf.extend(std::iter::repeat_n(b'a', MAX_REQUEST_LINE_BYTES));
        buf.extend_from_slice(b" HTTP/1.1\r\n");
        assert_eq!(parse_request_line(&buf), Err(RequestError::TooLarge));
    }

    #[test]
    fn rejects_wrong_part_counts() {
        for raw in [
            &b"GET /livez\r\n"[..],
            b"GET  /livez HTTP/1.1\r\n",
            b"GET /livez HTTP/1.1 extra\r\n",
        ] {
            assert_eq!(
                parse_request_line(raw),
                Err(RequestError::Malformed),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn rejects_bad_method_target_and_version() {
        for raw in [
            &b"get /livez HTTP/1.1\r\n"[..],
            b"GET livez HTTP/1.1\r\n",
            b"GET /livez HTTP/2\r\n",
            b"GET /livez SPDY/3\r\n",
        ] {
            assert_eq!(
                parse_request_line(raw),
                Err(RequestError::Malformed),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn rejects_non_utf8() {
        assert_eq!(
            parse_request_line(b"GET /\xff HTTP/1.1\r\n"),
            Err(RequestError::Malformed)
        );
    }

    #[test]
    fn respond_answers_400_without_calling_the_router() {
        let response = respond(b"\x16\x03\x01garbage", |_| {
            panic!("router must not see a malformed request")
        });
        assert_eq!(response.status, Status::BadRequest);
    }

    #[test]
    fn respond_hands_a_valid_line_to_the_router() {
        let response = respond(b"GET /x HTTP/1.1\r\n", |line| {
            Response::text(Status::Ok, line.path.to_string())
        });
        assert_eq!(response, Response::text(Status::Ok, "/x"));
    }

    #[test]
    fn response_bytes_carry_status_type_length_and_close() {
        let bytes = Response::text(Status::ServiceUnavailable, "starting").to_bytes();
        let text = String::from_utf8(bytes).expect("utf-8");
        assert!(text.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
        assert!(text.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(text.contains("Content-Length: 8\r\n"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.ends_with("\r\n\r\nstarting"));
    }

    #[test]
    fn status_codes_match_their_names() {
        assert_eq!(Status::Ok.code(), 200);
        assert_eq!(Status::BadRequest.code(), 400);
        assert_eq!(Status::NotFound.code(), 404);
        assert_eq!(Status::ServiceUnavailable.code(), 503);
    }

    #[tokio::test]
    async fn served_listener_answers_over_tcp() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = bind(0).await.expect("bind ephemeral port");
        let port = listener.local_addr().expect("addr").port();
        spawn_server(
            listener,
            "test",
            std::sync::Arc::new(|buf: &[u8]| {
                respond(buf, |line| {
                    Response::text(Status::Ok, line.path.to_string())
                })
            }),
        );

        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect");
        stream
            .write_all(b"GET /hello HTTP/1.1\r\n\r\n")
            .await
            .expect("write");
        let mut out = String::new();
        stream.read_to_string(&mut out).await.expect("read");
        assert!(out.starts_with("HTTP/1.1 200 OK\r\n"), "{out}");
        assert!(out.ends_with("/hello"), "{out}");
    }

    #[tokio::test]
    async fn bind_reports_a_port_in_use() {
        let first = bind(0).await.expect("bind ephemeral port");
        let port = first.local_addr().expect("addr").port();
        assert!(bind(port).await.is_err());
    }
}
