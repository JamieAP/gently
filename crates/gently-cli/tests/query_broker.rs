//! Desktop query delegation uses a private socket, never a token handoff.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::{fs::PermissionsExt, net::UnixStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Watcher(Child);
impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(state: &std::path::Path, url: &str) -> Watcher {
    Watcher(
        Command::new(assert_cmd::cargo::cargo_bin!("gently"))
            .args(["export", "--watch", "--serve-queries"])
            .env("GENTLY_STATE_DIR", state)
            .env("GENTLY_TENANT_ID", "personal")
            .env("GENTLY_DEVICE_ID", "capture-host")
            .env("GENTLY_TOKEN", "synthetic-broker-token")
            .env("GENTLY_COLLECTOR_URL", url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

fn wait_ready(watcher: &mut Watcher, path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o777 == 0o600)
        || UnixStream::connect(path).is_err()
    {
        assert!(
            watcher.0.try_wait().unwrap().is_none(),
            "watcher exited before creating query socket"
        );
        assert!(Instant::now() < deadline, "query socket never became ready");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn request(path: &std::path::Path, req: serde_json::Value) -> serde_json::Value {
    let mut stream = UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    if let Err(error) = writeln!(stream, "{req}") {
        // A bounded receiver can reject an oversized request and close before
        // the sender finishes writing. macOS can report NotConnected instead
        // of BrokenPipe; the bounded error response must still be readable.
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::NotConnected
        ));
    }
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn broker_reports_busy_and_recovers_after_slots_are_released() {
    let dir = tempfile::tempdir_in(if cfg!(target_os = "macos") {
        std::path::PathBuf::from("/private/tmp")
    } else {
        std::env::temp_dir()
    })
    .unwrap();
    let mut watcher = start(dir.path(), "http://127.0.0.1:9");
    let socket = dir
        .path()
        .join("tenants/personal/devices/capture-host/query.sock");
    wait_ready(&mut watcher, &socket);
    std::thread::sleep(Duration::from_millis(50));
    let holders: Vec<_> = (0..16)
        .map(|_| UnixStream::connect(&socket).unwrap())
        .collect();
    std::thread::sleep(Duration::from_millis(50));
    let req = serde_json::json!({
        "collector_url":"http://127.0.0.1:9", "tenant_id":"personal", "params":[["op","write"]]
    });
    assert_eq!(request(&socket, req.clone())["error"], "query broker busy");
    drop(holders);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let response = request(&socket, req.clone());
        if response["error"] == "query operation is not read-only" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "broker did not release completed slots"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(watcher.0.try_wait().unwrap().is_none());
}

#[test]
fn tokenless_cli_and_mcp_query_through_unlocked_watcher() {
    let dir = tempfile::tempdir_in(if cfg!(target_os = "macos") {
        std::path::PathBuf::from("/private/tmp")
    } else {
        std::env::temp_dir()
    })
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let collector = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut headers = String::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
            assert!(headers.starts_with("GET /v1/query?tenant_id=personal&op=traces"));
            assert!(headers
                .to_lowercase()
                .contains("authorization: bearer synthetic-broker-token"));
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]").unwrap();
        }
    });
    let mut watcher = start(dir.path(), &url);
    let socket = dir
        .path()
        .join("tenants/personal/devices/capture-host/query.sock");
    wait_ready(&mut watcher, &socket);
    assert_eq!(
        std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_cmd::Command::cargo_bin("gently")
        .unwrap()
        .args(["traces", "--json"])
        .env("GENTLY_STATE_DIR", dir.path())
        .env("GENTLY_TENANT_ID", "personal")
        .env("GENTLY_DEVICE_ID", "capture-host")
        .env_remove("GENTLY_TOKEN")
        .env("GENTLY_COLLECTOR_URL", &url)
        .assert()
        .success()
        .stdout("[]\n");
    let output = assert_cmd::Command::cargo_bin("gently").unwrap().arg("mcp")
        .env("GENTLY_STATE_DIR", dir.path())
        .env("GENTLY_TENANT_ID", "personal").env("GENTLY_DEVICE_ID", "capture-host").env_remove("GENTLY_TOKEN")
        .env("GENTLY_COLLECTOR_URL", &url)
        .write_stdin("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"list_traces\",\"arguments\":{}}}\n")
        .assert().success().get_output().stdout.clone();
    let response: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    assert!(!String::from_utf8(output)
        .unwrap()
        .contains("synthetic-broker-token"));
    collector.join().unwrap();
    let invalid = request(
        &socket,
        serde_json::json!({"tenant_id":"personal","collector_url":url,"params":[["op","delete"]]}),
    );
    assert!(invalid["error"].is_string());
    let wrong_target = request(
        &socket,
        serde_json::json!({"tenant_id":"personal","collector_url":"http://other.invalid","params":[["op","traces"]]}),
    );
    assert!(wrong_target["error"].is_string());
    let wrong_tenant = request(
        &socket,
        serde_json::json!({"tenant_id":"other","collector_url":url,"params":[["op","traces"]]}),
    );
    assert_eq!(
        wrong_tenant["error"],
        "query tenant differs from watcher tenant"
    );
    let tenant_override = request(
        &socket,
        serde_json::json!({"tenant_id":"personal","collector_url":url,"params":[["op","traces"],["tenant_id","other"]]}),
    );
    assert_eq!(tenant_override["error"], "unsupported query parameter");
    // Watcher's graceful shutdown removes the socket.
    Command::new("kill")
        .args(["-INT", &watcher.0.id().to_string()])
        .status()
        .unwrap();
    assert!(watcher.0.wait().unwrap().success());
    assert!(!socket.exists());
}

#[test]
fn query_socket_never_replaces_a_symlink_or_regular_file() {
    for symlink in [false, true] {
        let dir = tempfile::tempdir_in(if cfg!(target_os = "macos") {
            std::path::PathBuf::from("/private/tmp")
        } else {
            std::env::temp_dir()
        })
        .unwrap();
        let target = dir.path().join("keep");
        std::fs::write(&target, "unrelated data").unwrap();
        let socket = dir
            .path()
            .join("tenants/personal/devices/capture-host/query.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        if symlink {
            std::os::unix::fs::symlink(&target, &socket).unwrap();
        } else {
            std::fs::write(&socket, "unrelated data").unwrap();
        }
        let mut watcher = start(dir.path(), "http://127.0.0.1:9");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = watcher.0.try_wait().unwrap() {
                assert!(!status.success());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "watcher accepted a non-socket path"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut data = String::new();
        std::fs::File::open(&target)
            .unwrap()
            .read_to_string(&mut data)
            .unwrap();
        assert_eq!(data, "unrelated data");
        assert!(socket.exists());
    }
}

#[test]
fn stale_socket_recovers_and_invalid_queries_do_not_disable_the_broker() {
    use std::os::unix::net::UnixListener;
    let dir = tempfile::tempdir_in(if cfg!(target_os = "macos") {
        std::path::PathBuf::from("/private/tmp")
    } else {
        std::env::temp_dir()
    })
    .unwrap();
    let path = dir
        .path()
        .join("tenants/personal/devices/capture-host/query.sock");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let stale = UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    drop(stale);
    let url = "http://127.0.0.1:9";
    let mut watcher = start(dir.path(), url);
    wait_ready(&mut watcher, &path);
    for params in [
        serde_json::json!([["op", "traces"], ["op", "stats"]]),
        serde_json::json!([["op", "stats"], ["url", "http://other.invalid"]]),
        serde_json::json!([["op", "traces"], ["name", "x".repeat(70_000)]]),
    ] {
        let result = request(
            &path,
            serde_json::json!({"tenant_id":"personal","collector_url":url,"params":params}),
        );
        assert!(result["error"].is_string());
        assert!(!result.to_string().contains("synthetic-broker-token"));
    }
    let result = request(
        &path,
        serde_json::json!({"tenant_id":"personal","collector_url":url,"params":[["op","stats"]]}),
    );
    assert_eq!(result["error"], "collector query transport failed");
    assert!(watcher.0.try_wait().unwrap().is_none());
}

#[test]
fn retention_is_default_and_discard_is_explicit_after_authentication_failure() {
    for (flag, expected) in [
        (None, 3),
        (Some("--preserve-backlog"), 3),
        (Some("--discard-oldest"), 1),
    ] {
        let dir = tempfile::tempdir_in(if cfg!(target_os = "macos") {
            std::path::PathBuf::from("/private/tmp")
        } else {
            std::env::temp_dir()
        })
        .unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "outbox_cap = 1\nprefer_quic = false\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("tenants/personal/devices/capture-host")).unwrap();
        let store = gently_store::Store::open(
            &dir.path()
                .join("tenants/personal/devices/capture-host/state.db"),
        )
        .unwrap();
        for _ in 0..3 {
            store.outbox_enqueue("{\"resourceSpans\":[]}").unwrap();
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let collector = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut input = [0; 4096];
            assert!(stream.read(&mut input).unwrap() > 0);
            stream
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let mut cmd = assert_cmd::Command::cargo_bin("gently").unwrap();
        cmd.arg("export");
        if let Some(flag) = flag {
            cmd.arg(flag);
        }
        cmd.env("GENTLY_STATE_DIR", dir.path())
            .env("GENTLY_TENANT_ID", "personal")
            .env("GENTLY_DEVICE_ID", "capture-host")
            .env("GENTLY_TOKEN", "synthetic-broker-token")
            .env("GENTLY_COLLECTOR_URL", url)
            .assert()
            .failure()
            .stderr(predicates::str::contains("401"));
        collector.join().unwrap();
        assert_eq!(store.outbox_len().unwrap(), expected);
    }
}

#[test]
fn malformed_collector_fields_never_reach_tokenless_cli_diagnostics() {
    let dir = tempfile::tempdir_in(if cfg!(target_os = "macos") {
        std::path::PathBuf::from("/private/tmp")
    } else {
        std::env::temp_dir()
    })
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let collector = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
        }
        let body = r#"[{"span_count":"synthetic-private-decode-canary"}]"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let mut watcher = start(dir.path(), &url);
    let socket = dir
        .path()
        .join("tenants/personal/devices/capture-host/query.sock");
    wait_ready(&mut watcher, &socket);
    let output = assert_cmd::Command::cargo_bin("gently")
        .unwrap()
        .args(["traces", "--json"])
        .env("GENTLY_STATE_DIR", dir.path())
        .env("GENTLY_TENANT_ID", "personal")
        .env("GENTLY_DEVICE_ID", "capture-host")
        .env("GENTLY_COLLECTOR_URL", &url)
        .env_remove("GENTLY_TOKEN")
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid local query response"));
    for bytes in [&output.stdout, &output.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains("synthetic-private-decode-canary"));
    }
    collector.join().unwrap();
}
