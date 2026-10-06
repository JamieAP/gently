use assert_cmd::Command;
use gently_store::Store;

fn export_auth_failure(discard: bool) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "prefer_quic = false\noutbox_cap = 1\nexport_timeout_secs = 2\n",
    )
    .unwrap();
    let db = dir
        .path()
        .join("tenants/personal/devices/synthetic-host/state.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let store = Store::open(&db).unwrap();
    let envelope = gently_core::OtlpRequest::single(
        &gently_core::Resource::new("synthetic", "codex", "/synthetic"),
        vec![],
    );
    for _ in 0..3 {
        store
            .outbox_enqueue(&serde_json::to_string(&envelope).unwrap())
            .unwrap();
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
            assert!(headers.len() < 16 * 1024);
        }
        let length: usize = String::from_utf8(headers)
            .unwrap()
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse().unwrap())
            })
            .unwrap();
        stream.read_exact(&mut vec![0; length]).unwrap();
        stream
            .write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
    });
    let mut command = Command::cargo_bin("gently").unwrap();
    command.arg("export");
    if discard {
        command.arg("--discard-oldest");
    }
    command
        .env("GENTLY_STATE_DIR", dir.path())
        .env("GENTLY_TENANT_ID", "personal")
        .env("GENTLY_DEVICE_ID", "synthetic-host")
        .env("GENTLY_COLLECTOR_URL", format!("http://{addr}"))
        .env("GENTLY_TOKEN", "synthetic-auth")
        .env("GENTLY_SYNC_RAW_VALUES", "0")
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "authentication failed (HTTP 401)",
        ));
    server.join().unwrap();
    assert_eq!(store.outbox_len().unwrap(), if discard { 1 } else { 3 });
}

#[test]
fn destructive_trim_requires_explicit_option_and_conflicts_with_preserve() {
    Command::cargo_bin("gently")
        .unwrap()
        .args(["export", "--discard-oldest", "--preserve-backlog"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn default_export_retains_over_cap_queue_on_authentication_failure() {
    export_auth_failure(false);
}
#[test]
fn explicit_discard_trims_even_when_authentication_fails() {
    export_auth_failure(true);
}
