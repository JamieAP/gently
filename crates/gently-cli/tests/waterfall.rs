use assert_cmd::Command;
use predicates::str::contains;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

fn gently(home: &tempfile::TempDir) -> Command {
    let mut command = Command::cargo_bin("gently").unwrap();
    command.env_clear().env("HOME", home.path());
    command
}

fn spans() -> String {
    serde_json::json!([
        {
            "span_id": "root", "trace_id": "trace", "name": "session",
            "kind": 1, "start_unix_nano": "1000000000",
            "end_unix_nano": "2000000000", "status": 1
        },
        {
            "span_id": "tool", "trace_id": "trace", "parent_span_id": "root",
            "name": "Bash", "kind": 1, "start_unix_nano": "1100000000",
            "end_unix_nano": "1300000000", "status": 2
        }
    ])
    .to_string()
}

#[test]
fn waterfall_renders_stdin_without_configuration() {
    let home = tempfile::tempdir().unwrap();
    gently(&home)
        .arg("waterfall")
        .write_stdin(spans())
        .assert()
        .success()
        .stdout(contains("1000.0ms ✓  session"))
        .stdout(contains("200.0ms ✗    Bash"))
        .stdout(contains("INTEGRITY"))
        .stdout(contains("parent links resolved       : PASS ✓"));
    assert!(!home.path().join(".gently").exists());
}

#[test]
fn waterfall_rejects_empty_input() {
    let home = tempfile::tempdir().unwrap();
    gently(&home)
        .arg("waterfall")
        .write_stdin("[]")
        .assert()
        .failure()
        .stderr(contains("no spans"));
}

#[test]
fn waterfall_rejects_invalid_json() {
    let home = tempfile::tempdir().unwrap();
    gently(&home)
        .arg("waterfall")
        .write_stdin("not JSON")
        .assert()
        .failure()
        .stderr(contains("read span array"));
}

#[test]
fn trace_waterfall_and_json_are_mutually_exclusive() {
    let home = tempfile::tempdir().unwrap();
    gently(&home)
        .args(["trace", "trace", "--waterfall", "--json"])
        .assert()
        .code(2)
        .stderr(contains("cannot be used with"));
}

#[test]
fn trace_waterfall_queries_the_collector() {
    let home = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let body = spans();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "collector request timed out");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("collector accept: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0 && request.len() < 16_384);
            request.extend_from_slice(&buffer[..count]);
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("GET /v1/query?tenant_id=personal&op=trace&trace_id=trace "));
        assert!(request
            .to_lowercase()
            .contains("authorization: bearer t\r\n"));
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let output = gently(&home)
        .args(["trace", "trace", "--waterfall"])
        .env("GENTLY_COLLECTOR_URL", format!("http://{address}"))
        .env("GENTLY_TOKEN", "t")
        .env("GENTLY_TENANT_ID", "personal")
        .assert();
    server.join().unwrap();
    output
        .success()
        .stdout(contains("1000.0ms ✓  session"))
        .stdout(contains("INTEGRITY"));
}
