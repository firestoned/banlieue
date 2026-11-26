// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `http.rs`.

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn get_request_is_encoded_with_host_and_close() {
        let req = Request {
            method: "GET",
            path: "/machine/?comp=goalstate",
            headers: &[("x-ms-version", "2012-11-30")],
            body: None,
        };
        let got = String::from_utf8(encode_request(&req, "192.0.2.1")).expect("utf8");
        assert_eq!(
            got,
            "GET /machine/?comp=goalstate HTTP/1.1\r\nHost: 192.0.2.1\r\nConnection: close\r\nx-ms-version: 2012-11-30\r\n\r\n"
        );
    }

    #[test]
    fn post_request_carries_content_length_and_body() {
        let req = Request {
            method: "POST",
            path: "/p",
            headers: &[],
            body: Some(b"abc"),
        };
        let got = String::from_utf8(encode_request(&req, "h")).expect("utf8");
        assert!(got.ends_with("Content-Length: 3\r\n\r\nabc"));
    }

    #[test]
    fn content_length_body_is_parsed() {
        let r = parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi").expect("ok");
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"hi");
        assert!(r.is_success());
    }

    #[test]
    fn chunked_body_is_dechunked() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n";
        assert_eq!(parse_response(raw).expect("ok").body, b"abcde");
    }

    #[test]
    fn a_body_without_framing_runs_to_the_end() {
        let r = parse_response(b"HTTP/1.0 410 Gone\r\n\r\nrest").expect("ok");
        assert_eq!(r.status, 410);
        assert_eq!(r.body, b"rest");
        assert!(!r.is_success());
    }

    #[test]
    fn malformed_responses_are_refused() {
        for raw in [
            &b"HTTP/1.1 200 OK\r\n"[..],
            b"SSH-2.0-x\r\n\r\n",
            b"HTTP/1.1 abc OK\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nbad header\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nshort",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab\r\n",
        ] {
            assert!(
                matches!(parse_response(raw), Err(HttpError::Malformed(_))),
                "{:?}",
                String::from_utf8_lossy(raw)
            );
        }
    }

    #[test]
    fn send_talks_to_a_local_server() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let n = s.read(&mut buf).expect("read");
            s.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\nok")
                .expect("write");
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });
        let req = Request {
            method: "GET",
            path: "/x",
            headers: &[],
            body: None,
        };
        let r = send(addr, &req, Duration::from_secs(5)).expect("sent");
        assert_eq!(r.status, 201);
        assert!(
            server
                .join()
                .expect("join")
                .starts_with("GET /x HTTP/1.1\r\n")
        );
    }

    #[test]
    fn send_refuses_an_oversized_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\n\r\n");
            let _ = s.write_all(&vec![b'a'; MAX_RESPONSE_BYTES + 1]);
        });
        let req = Request {
            method: "GET",
            path: "/x",
            headers: &[],
            body: None,
        };
        assert!(matches!(
            send(addr, &req, Duration::from_secs(5)),
            Err(HttpError::TooLarge)
        ));
        server.join().expect("join");
    }

    #[test]
    fn send_to_a_closed_port_is_an_io_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        let req = Request {
            method: "GET",
            path: "/x",
            headers: &[],
            body: None,
        };
        assert!(matches!(
            send(addr, &req, Duration::from_secs(1)),
            Err(HttpError::Io(_))
        ));
    }
}
