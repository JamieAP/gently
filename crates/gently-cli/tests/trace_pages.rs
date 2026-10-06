use assert_cmd::Command;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

fn row(id: &str) -> Value {
    json!({"span_id":id,"trace_id":"synthetic","name":"synthetic","kind":1,
        "start_unix_nano":"1","end_unix_nano":"2","status":0,"attrs_json":"[]"})
}
fn page(id: &str, cursor: Option<&str>) -> Value {
    json!({"rows":[row(id)],"next_cursor":cursor,"complete":cursor.is_none()})
}
fn ok(body: Value) -> (&'static str, Value) {
    ("200 OK", body)
}

/// Serve one response per request, in order, then stop listening, and run
/// `gently trace` against it. Returns the CLI result and each request line.
fn trace_with(responses: Vec<(&'static str, Value)>) -> (assert_cmd::assert::Assert, Vec<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, body) in responses {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() > deadline {
                            return requests;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("synthetic collector: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 16 * 1024);
            }
            requests.push(
                String::from_utf8(headers)
                    .unwrap()
                    .lines()
                    .next()
                    .unwrap()
                    .to_owned(),
            );
            let body = body.to_string();
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
        requests
    });
    let home = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("gently")
        .unwrap()
        .env_clear()
        .env("HOME", home.path())
        .args(["trace", "synthetic", "--json"])
        .env("GENTLY_COLLECTOR_URL", format!("http://{address}"))
        .env("GENTLY_TOKEN", "synthetic-auth")
        .env("GENTLY_TENANT_ID", "personal")
        .assert();
    (output, server.join().unwrap())
}

fn span_ids(output: assert_cmd::assert::Assert) -> Vec<String> {
    let rows: Vec<Value> = serde_json::from_slice(&output.success().get_output().stdout).unwrap();
    rows.iter()
        .map(|row| row["span_id"].as_str().unwrap().to_owned())
        .collect()
}

fn fails_with(output: assert_cmd::assert::Assert, message: &str) {
    output
        .failure()
        .stdout(predicates::str::is_empty())
        .stderr(predicates::str::contains(message));
}

#[test]
fn assembles_complete_cursor_pages() {
    let (output, requests) = trace_with(vec![
        ok(page("1", Some("next"))),
        ok(page("2", Some("final"))),
        ok(page("3", None)),
    ]);
    assert_eq!(span_ids(output), ["1", "2", "3"]);
    assert!(requests[0].contains("page=1") && !requests[0].contains("cursor="));
    assert!(requests[1].contains("cursor=next"));
    assert!(requests[2].contains("cursor=final"));
}

#[test]
fn rejects_a_repeated_cursor_without_requesting_it_again() {
    let (output, requests) = trace_with(vec![
        ok(page("1", Some("next"))),
        ok(page("2", Some("next"))),
    ]);
    fails_with(output, "invalid or repeated trace cursor");
    assert_eq!(requests.len(), 2);
}

#[test]
fn rejects_inconsistent_completion() {
    for body in [
        json!({"rows":[row("1")],"next_cursor":null,"complete":false}),
        json!({"rows":[row("1")],"next_cursor":"next","complete":true}),
    ] {
        let (output, requests) = trace_with(vec![ok(body)]);
        fails_with(output, "inconsistent trace pagination response");
        assert_eq!(requests.len(), 1);
    }
}

#[test]
fn rejects_rows_from_another_trace() {
    let mut body = page("1", None);
    body["rows"][0]["trace_id"] = "other".into();
    let (output, _) = trace_with(vec![ok(body)]);
    fails_with(output, "collector returned a different trace");
}

#[test]
fn restarts_the_read_when_the_trace_changes() {
    let conflict = (
        "409 Conflict",
        json!({"error":"Trace changed during pagination; repeat the query"}),
    );
    let (output, requests) = trace_with(vec![
        ok(page("1", Some("next"))),
        conflict,
        ok(page("1", Some("again"))),
        ok(page("2", None)),
    ]);
    assert_eq!(span_ids(output), ["1", "2"]);
    assert_eq!(requests.len(), 4);
    assert!(!requests[2].contains("cursor="));
}

#[test]
fn gives_up_after_three_reads_that_each_see_a_change() {
    // A span repeated across pages is the same change, detected by the CLI.
    let attempt = || [ok(page("1", Some("next"))), ok(page("1", None))];
    let (output, requests) = trace_with(
        attempt()
            .into_iter()
            .chain(attempt())
            .chain(attempt())
            .collect(),
    );
    fails_with(output, "trace changed during pagination on 3 attempts");
    assert_eq!(requests.len(), 6);
}

#[test]
fn reports_the_collectors_reason_for_a_rejected_page() {
    let (output, _) = trace_with(vec![(
        "413 Payload Too Large",
        json!({"error":"A trace row exceeds the bounded page budget"}),
    )]);
    fails_with(
        output,
        "collector query returned HTTP 413: A trace row exceeds the bounded page budget",
    );
}

#[cfg(unix)]
#[test]
fn old_watcher_parameter_rejection_retries_legacy_once_without_retrying_other_failures() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};
    for error in [
        "unsupported query parameter",
        "query tenant differs from watcher tenant",
    ] {
        let home = tempfile::Builder::new()
            .prefix("gp-")
            .tempdir_in("/tmp")
            .unwrap();
        let runtime = home
            .path()
            .join(".gently/tenants/personal/devices/synthetic");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = runtime.join("query.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let fallback = error == "unsupported query parameter";
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for index in 0..if fallback { 2 } else { 1 } {
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline);
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("synthetic broker: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = String::new();
                BufReader::new(&mut stream).read_line(&mut request).unwrap();
                requests.push(serde_json::from_str::<Value>(&request).unwrap());
                let response = if index == 0 {
                    json!({"error":error})
                } else {
                    json!({"result":[row("legacy")]})
                };
                writeln!(stream, "{response}").unwrap();
            }
            requests
        });
        let output = Command::cargo_bin("gently")
            .unwrap()
            .env_clear()
            .env("HOME", home.path())
            .env("GENTLY_COLLECTOR_URL", "http://127.0.0.1:8787")
            .env("GENTLY_TENANT_ID", "personal")
            .env("GENTLY_DEVICE_ID", "synthetic")
            .args(["trace", "synthetic", "--json"])
            .assert();
        if fallback {
            output.success().stdout(predicates::str::contains("legacy"));
        } else {
            output.failure().stderr(predicates::str::contains(error));
        }
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), if fallback { 2 } else { 1 });
        assert!(requests[0]["params"]
            .as_array()
            .unwrap()
            .iter()
            .any(|param| param[0] == "page"));
        if fallback {
            assert!(requests[1]["params"]
                .as_array()
                .unwrap()
                .iter()
                .all(|param| param[0] != "page"));
        }
    }
}
