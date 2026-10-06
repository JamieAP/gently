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
fn query(pages: Vec<Value>, success: bool) -> Vec<Value> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for page in pages {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline);
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
            let body = page.to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
    let requests = server.join().unwrap();
    assert!(requests[0].contains("page=1"));
    if requests.len() > 1 {
        assert!(requests[1].contains("cursor=next"));
    }
    if success {
        let output = output.success();
        serde_json::from_slice(&output.get_output().stdout).unwrap()
    } else {
        output.failure().stdout(predicates::str::is_empty());
        vec![]
    }
}
#[test]
fn assembles_complete_cursor_pages() {
    let rows = query(
        vec![
            page("1", Some("next")),
            page("2", Some("final")),
            page("3", None),
        ],
        true,
    );
    assert_eq!(
        rows.iter()
            .map(|row| row["span_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["1", "2", "3"]
    );
}
#[test]
fn rejects_repeated_cursors_duplicate_rows_and_inconsistent_completion() {
    query(
        vec![page("1", Some("next")), page("2", Some("next"))],
        false,
    );
    query(vec![page("1", Some("next")), page("1", None)], false);
    query(
        vec![json!({"rows":[row("1")],"next_cursor":null,"complete":false})],
        false,
    );
}
#[test]
fn rejects_rows_from_another_trace() {
    let mut body = page("1", None);
    body["rows"][0]["trace_id"] = "other".into();
    query(vec![body], false);
}
