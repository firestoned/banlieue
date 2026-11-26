// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `ready.rs`, against a fake WireServer on 127.0.0.1.

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::azure::{ACTIVE_MODE_FILE, AZURE_CHASSIS_ASSET_TAG, RECOVERY_MODE_FILE};
    use std::fs;
    use std::io::{self, Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    const GOAL_STATE: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?><GoalState><Version>2012-11-30</Version><Incarnation>2</Incarnation><Container><ContainerId>00000000-0000-4000-8000-000000000001</ContainerId><RoleInstanceList><RoleInstance><InstanceId>00000000000000000000000000000002._az-01</InstanceId></RoleInstance></RoleInstanceList></Container></GoalState>";

    /// A fake WireServer: answers each connection with the next scripted
    /// response and records every request it saw.
    struct FakeWireServer {
        addr: SocketAddr,
        requests: Arc<Mutex<Vec<String>>>,
    }

    fn reply(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    fn read_request(s: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = s.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf);
            if let Some(head_end) = text.find("\r\n\r\n") {
                let len = text[..head_end]
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Length: "))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= head_end + 4 + len {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn fake(script: Vec<String>) -> FakeWireServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        std::thread::spawn(move || {
            for response in script {
                let Ok((mut s, _)) = listener.accept() else {
                    return;
                };
                let req = read_request(&mut s);
                seen.lock().expect("lock").push(req);
                let _ = s.write_all(response.as_bytes());
                let _ = s.shutdown(Shutdown::Write);
            }
        });
        FakeWireServer { addr, requests }
    }

    fn fast() -> RetryPolicy {
        RetryPolicy {
            initial: Duration::from_millis(1),
            max: Duration::from_millis(4),
            deadline: Duration::from_secs(5),
            request_timeout: Duration::from_secs(2),
        }
    }

    fn quiet() -> Logger {
        Logger::to(Box::new(io::sink()))
    }

    struct Guest {
        _root: tempfile::TempDir,
        tag: std::path::PathBuf,
        modes: std::path::PathBuf,
    }

    fn guest(on_azure: bool, mode_file: Option<&str>) -> Guest {
        let root = tempfile::tempdir().expect("tempdir");
        let tag = root.path().join("chassis_asset_tag");
        let value = if on_azure {
            AZURE_CHASSIS_ASSET_TAG
        } else {
            "No Asset Tag"
        };
        fs::write(&tag, format!("{value}\n")).expect("tag");
        let modes = root.path().join("cos");
        fs::create_dir_all(&modes).expect("cos");
        if let Some(m) = mode_file {
            fs::write(modes.join(m), "").expect("mode");
        }
        Guest {
            _root: root,
            tag,
            modes,
        }
    }

    // ---- constants ----

    #[test]
    fn the_address_is_wireserver_on_port_80() {
        assert_eq!(wireserver_addr().to_string(), "168.63.129.16:80");
    }

    #[test]
    fn backoff_doubles_up_to_the_ceiling() {
        let max = Duration::from_secs(30);
        assert_eq!(
            next_backoff(Duration::from_secs(1), max),
            Duration::from_secs(2)
        );
        assert_eq!(next_backoff(Duration::from_secs(20), max), max);
        assert_eq!(next_backoff(max, max), max);
    }

    #[test]
    fn the_system_deadline_fits_inside_azures_provisioning_window() {
        let window = Duration::from_secs(20 * 60);
        assert!(RetryPolicy::system().deadline < window);
    }

    // ---- one attempt ----

    #[test]
    fn report_once_gets_the_goal_state_then_posts_ready() {
        let ws = fake(vec![reply("200 OK", GOAL_STATE), reply("200 OK", "")]);
        report_once(ws.addr, Duration::from_secs(2)).expect("reported");
        let reqs = ws.requests.lock().expect("lock").clone();
        assert_eq!(reqs.len(), 2);
        assert!(reqs[0].starts_with("GET /machine/?comp=goalstate HTTP/1.1\r\n"));
        assert!(reqs[0].contains("x-ms-version: 2012-11-30\r\n"));
        assert!(reqs[0].contains("x-ms-agent-name: banlieue-azure-guest\r\n"));
        assert!(reqs[1].starts_with("POST /machine/?comp=health HTTP/1.1\r\n"));
        assert!(reqs[1].contains("Content-Type: text/xml;charset=utf-8\r\n"));
        assert!(reqs[1].contains("<GoalStateIncarnation>2</GoalStateIncarnation>"));
        assert!(reqs[1].contains("<State>Ready</State>"));
    }

    #[test]
    fn a_refused_health_post_is_a_status_error() {
        let ws = fake(vec![
            reply("200 OK", GOAL_STATE),
            reply("400 Bad Request", ""),
        ]);
        let err = report_once(ws.addr, Duration::from_secs(2)).expect_err("refused");
        assert!(matches!(
            err,
            AttemptError::Status {
                what: "health",
                status: 400
            }
        ));
    }

    #[test]
    fn a_bad_goal_state_is_never_posted() {
        let ws = fake(vec![reply("200 OK", "<GoalState/>"), reply("200 OK", "")]);
        let err = report_once(ws.addr, Duration::from_secs(2)).expect_err("refused");
        assert!(matches!(err, AttemptError::GoalState(_)));
        assert_eq!(ws.requests.lock().expect("lock").len(), 1);
    }

    // ---- retries ----

    #[test]
    fn retries_until_wireserver_accepts() {
        let ws = fake(vec![
            reply("503 Service Unavailable", ""),
            reply("200 OK", GOAL_STATE),
            reply("500 Internal Server Error", ""),
            reply("200 OK", GOAL_STATE),
            reply("200 OK", ""),
        ]);
        let mut slept = Vec::new();
        let attempts =
            report_ready(ws.addr, &fast(), &mut quiet(), |d| slept.push(d)).expect("reported");
        assert_eq!(attempts, 3);
        assert_eq!(slept, [Duration::from_millis(1), Duration::from_millis(2)]);
    }

    #[test]
    fn gives_up_at_the_deadline_when_nothing_answers() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);
        let policy = RetryPolicy {
            deadline: Duration::from_millis(50),
            ..fast()
        };
        let err =
            report_ready(addr, &policy, &mut quiet(), std::thread::sleep).expect_err("deadline");
        assert!(err.attempts >= 1);
        assert!(err.last.contains("connection"));
    }

    // ---- the gates ----

    #[test]
    fn off_azure_nothing_is_sent() {
        let ws = fake(vec![]);
        let g = guest(false, Some(ACTIVE_MODE_FILE));
        let outcome =
            run_with(&g.tag, &g.modes, ws.addr, &fast(), &mut quiet(), |_| {}).expect("ok");
        assert_eq!(outcome, Outcome::NotAzure);
        assert!(ws.requests.lock().expect("lock").is_empty());
    }

    #[test]
    fn recovery_never_reports() {
        let ws = fake(vec![]);
        let g = guest(true, Some(RECOVERY_MODE_FILE));
        let outcome =
            run_with(&g.tag, &g.modes, ws.addr, &fast(), &mut quiet(), |_| {}).expect("ok");
        assert_eq!(outcome, Outcome::NotInstalledBoot(BootMode::Recovery));
        assert!(ws.requests.lock().expect("lock").is_empty());
    }

    #[test]
    fn a_boot_without_a_kairos_marker_never_reports() {
        let ws = fake(vec![]);
        let g = guest(true, None);
        let outcome =
            run_with(&g.tag, &g.modes, ws.addr, &fast(), &mut quiet(), |_| {}).expect("ok");
        assert_eq!(outcome, Outcome::NotInstalledBoot(BootMode::Unknown));
    }

    #[test]
    fn an_active_boot_on_azure_reports() {
        let ws = fake(vec![reply("200 OK", GOAL_STATE), reply("200 OK", "")]);
        let g = guest(true, Some(ACTIVE_MODE_FILE));
        let outcome =
            run_with(&g.tag, &g.modes, ws.addr, &fast(), &mut quiet(), |_| {}).expect("ok");
        assert_eq!(outcome, Outcome::Reported(1));
    }
}
