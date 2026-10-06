//! Synthetic end-to-end coverage; never opens the user's vault or live state.
use age::secrecy::ExposeSecret;
use assert_cmd::Command;
use gently_raw::{DeviceIdentity, Manifest, OwnerKey, Recipient, TrustPin};
use gently_store::Store;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};

const CANARY: &str = "synthetic-private-roundtrip-canary";

#[derive(Default)]
struct Cloud {
    objects: BTreeMap<String, Value>,
    rows: Vec<Value>,
    fetched: usize,
}

fn request(stream: &mut TcpStream) -> (String, String, Vec<u8>) {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut block = [0; 8192];
        let n = stream.read(&mut block).unwrap();
        assert!(n > 0, "request ended before headers");
        bytes.extend_from_slice(&block[..n]);
        if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = String::from_utf8(bytes[..boundary].to_vec()).unwrap();
    let length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .map(|n| n.parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while bytes.len() < boundary + length {
        let mut block = [0; 8192];
        let n = stream.read(&mut block).unwrap();
        assert!(n > 0, "request ended before body");
        bytes.extend_from_slice(&block[..n]);
    }
    let mut first = headers.lines().next().unwrap().split_whitespace();
    let method = first.next().unwrap().to_owned();
    let path = first.next().unwrap().to_owned();
    assert!(path.contains("tenant_id=personal"));
    assert!(headers
        .to_ascii_lowercase()
        .contains("authorization: bearer synthetic-auth"));
    assert!(!String::from_utf8_lossy(&bytes).contains(CANARY));
    (method, path, bytes[boundary..boundary + length].to_vec())
}

fn command(state: &Path, device: &str, base: &str) -> Command {
    let mut cmd = Command::cargo_bin("gently").unwrap();
    cmd.env("GENTLY_STATE_DIR", state)
        .env("GENTLY_TENANT_ID", "personal")
        .env("GENTLY_DEVICE_ID", device)
        .env("GENTLY_COLLECTOR_URL", base)
        .env("GENTLY_TOKEN", "synthetic-auth")
        .env("GENTLY_CAPTURE_RAW_VALUES", "0")
        .env("GENTLY_SYNC_RAW_VALUES", "0")
        .env("GENTLY_RESOLVE_RAW_VALUES", "0")
        .env_remove("GENTLY_RAW_IDENTITY")
        .env_remove("GENTLY_RAW_MANIFEST")
        .env_remove("GENTLY_RAW_TRUST");
    cmd
}

#[test]
fn capture_export_remote_fetch_and_enrolled_reader_decrypt_without_plaintext_persistence() {
    let capture = tempfile::tempdir().unwrap();
    let reader = tempfile::tempdir_in(if cfg!(target_os = "macos") {
        std::path::PathBuf::from("/private/tmp")
    } else {
        std::env::temp_dir()
    })
    .unwrap();
    for state in [capture.path(), reader.path()] {
        std::fs::write(
            state.join("config.toml"),
            "prefer_quic = false\nexport_timeout_secs = 2\nquery_timeout_secs = 2\n",
        )
        .unwrap();
    }
    let identity = DeviceIdentity::generate();
    let owner = OwnerKey::generate();
    let signed = gently_raw::sign_manifest(
        Manifest {
            version: 1,
            tenant_id: "personal".into(),
            key_epoch: 1,
            expires_unix_secs: 4_102_444_800,
            readers: vec![Recipient {
                device_id: "reader-host".into(),
                key_id: "reader-key".into(),
                recipient: identity.to_public().to_string(),
            }],
        },
        &owner,
    )
    .unwrap();
    let pin = TrustPin {
        tenant_id: "personal".into(),
        owner_verify_key_b64: owner.verification_key_b64(),
        min_epoch: 1,
        manifest_digest: gently_raw::manifest_digest(&signed.manifest).unwrap(),
    };
    std::fs::write(
        capture.path().join("manifest.json"),
        serde_json::to_vec(&signed).unwrap(),
    )
    .unwrap();
    std::fs::write(
        capture.path().join("trust.json"),
        serde_json::to_vec(&pin).unwrap(),
    )
    .unwrap();
    // This disposable synthetic native key makes the test noninteractive.
    let identity_path = reader.path().join("synthetic-reader.agekey");
    std::fs::write(&identity_path, identity.to_string().expose_secret()).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cloud = Arc::new(Mutex::new(Cloud::default()));
    let shared = cloud.clone();
    // Two event ciphertext uploads, one metadata upload, one query, two cache-miss downloads.
    let server = std::thread::spawn(move || {
        for _ in 0..6 {
            let (mut stream, _) = listener.accept().unwrap();
            let (method, path, body) = request(&mut stream);
            let mut cloud = shared.lock().unwrap();
            let response = if method == "POST" && path.starts_with("/v1/raw-values?") {
                let object: Value = serde_json::from_slice(&body).unwrap();
                cloud.objects.insert(
                    object["context"]["raw_ref"].as_str().unwrap().into(),
                    object,
                );
                json!({})
            } else if method == "POST" && path.starts_with("/v1/traces?") {
                let otlp: Value = serde_json::from_slice(&body).unwrap();
                for resource in otlp["resourceSpans"].as_array().unwrap() {
                    for scope in resource["scopeSpans"].as_array().unwrap() {
                        for span in scope["spans"].as_array().unwrap() {
                            cloud.rows.push(json!({
                                "span_id":span["spanId"],"trace_id":span["traceId"],"parent_span_id":span["parentSpanId"],
                                "name":span["name"],"kind":span["kind"],"start_unix_nano":span["startTimeUnixNano"],
                                "end_unix_nano":span["endTimeUnixNano"],"status":span["status"]["code"],
                                "session_id":"roundtrip","harness":"claude-code","tool_name":null,"tool_use_id":null,
                                "resource_json":serde_json::to_string(&resource["resource"]["attributes"]).unwrap(),
                                "attrs_json":serde_json::to_string(&span["attributes"]).unwrap()
                            }));
                        }
                    }
                }
                json!({})
            } else if path.starts_with("/v1/query?") {
                json!(cloud.rows)
            } else {
                assert!(path.starts_with("/v1/raw-values/"));
                cloud.fetched += 1;
                let reference = path.split('/').nth(3).unwrap().split('?').next().unwrap();
                cloud.objects[reference].clone()
            };
            let response = serde_json::to_vec(&response).unwrap();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",response.len()).unwrap();
            stream.write_all(&response).unwrap();
        }
    });

    for payload in [
        json!({"hook_event_name":"UserPromptSubmit","session_id":"roundtrip","prompt":CANARY}),
        json!({"hook_event_name":"Stop","session_id":"roundtrip"}),
    ] {
        command(capture.path(), "capture-host", &base)
            .arg("hook")
            .env_remove("GENTLY_TOKEN")
            .env("GENTLY_CAPTURE_RAW_VALUES", "1")
            .env("GENTLY_RAW_MANIFEST", capture.path().join("manifest.json"))
            .env("GENTLY_RAW_TRUST", capture.path().join("trust.json"))
            .write_stdin(serde_json::to_string(&payload).unwrap())
            .assert()
            .success()
            .stdout(predicates::str::is_empty());
    }
    command(capture.path(), "capture-host", &base)
        .arg("export")
        .env("GENTLY_SYNC_RAW_VALUES", "1")
        .assert()
        .success();
    #[cfg(unix)]
    let _watcher = {
        // An unlocked watcher supplies read-only transport to the tokenless reader.
        struct Watcher(std::process::Child);
        impl Drop for Watcher {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut watcher = Watcher(
            std::process::Command::new(assert_cmd::cargo::cargo_bin!("gently"))
                .args(["export", "--watch", "--serve-queries"])
                .env("GENTLY_STATE_DIR", reader.path())
                .env("GENTLY_TENANT_ID", "personal")
                .env("GENTLY_DEVICE_ID", "reader-host")
                .env("GENTLY_COLLECTOR_URL", &base)
                .env("GENTLY_TOKEN", "synthetic-auth")
                .env("GENTLY_CAPTURE_RAW_VALUES", "0")
                .env("GENTLY_SYNC_RAW_VALUES", "0")
                .env("GENTLY_RESOLVE_RAW_VALUES", "0")
                .env_remove("GENTLY_RAW_IDENTITY")
                .env_remove("GENTLY_RAW_MANIFEST")
                .env_remove("GENTLY_RAW_TRUST")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let socket = reader
            .path()
            .join("tenants/personal/devices/reader-host/query.sock");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            assert!(watcher.0.try_wait().unwrap().is_none());
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        watcher
    };
    let mut reader_command = command(reader.path(), "reader-host", &base);
    #[cfg(unix)]
    reader_command.env_remove("GENTLY_TOKEN");
    let result = reader_command
        .args(["spans", "--json"])
        .env("GENTLY_RESOLVE_RAW_VALUES", "1")
        .env("GENTLY_RAW_IDENTITY", &identity_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let result = String::from_utf8(result).unwrap();
    assert!(result.contains(CANARY));
    server.join().unwrap();
    let cloud = cloud.lock().unwrap();
    assert_eq!(cloud.objects.len(), 2);
    assert_eq!(cloud.fetched, 2);
    assert!(!serde_json::to_string(&cloud.objects)
        .unwrap()
        .contains(CANARY));
    for (state, device) in [
        (capture.path(), "capture-host"),
        (reader.path(), "reader-host"),
    ] {
        let runtime = state.join(format!("tenants/personal/devices/{device}"));
        let store = Store::open(&runtime.join("state.db")).unwrap();
        assert_eq!(store.raw_objects_len().unwrap(), 2);
        assert!(store.raw_objects_pending("personal", 1).unwrap().is_empty());
        for file in std::fs::read_dir(runtime).unwrap() {
            let file = file.unwrap();
            if file.file_type().unwrap().is_file() {
                let bytes = std::fs::read(file.path()).unwrap();
                assert!(!bytes
                    .windows(CANARY.len())
                    .any(|part| part == CANARY.as_bytes()));
            }
        }
    }
}
