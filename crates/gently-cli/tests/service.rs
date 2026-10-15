#![cfg(unix)]
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::{fs::PermissionsExt, net::UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Controller(Child);
impl Drop for Controller {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGTERM);
        }
        let _ = self.0.wait();
    }
}
fn command(state: &Path) -> Command {
    let mut c = Command::new(assert_cmd::cargo::cargo_bin!("gently"));
    c.env_clear()
        .env("HOME", state)
        .env("GENTLY_STATE_DIR", state)
        .env("GENTLY_COLLECTOR_URL", "http://127.0.0.1:9")
        .env("GENTLY_TENANT_ID", "test")
        .env("GENTLY_DEVICE_ID", "host")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}
fn start(state: &Path) -> Controller {
    Controller(
        command(state)
            .args(["service", "run"])
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}
fn socket(state: &Path) -> PathBuf {
    state.join("tenants/test/devices/host/service.sock")
}
fn wait_ready(child: &mut Controller, state: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(socket(state)).is_err() {
        assert!(child.0.try_wait().unwrap().is_none(), "controller exited");
        assert!(Instant::now() < deadline, "controller did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn request(state: &Path, operation: &str) -> Value {
    let mut v = json!({"operation":operation,"collector_url":"http://127.0.0.1:9","tenant":"test","device":"host"});
    if operation == "unlock" {
        v["ingest"] = json!("synthetic-ingest");
        v["read"] = json!("synthetic-read");
    }
    raw_request(state, &serde_json::to_vec(&v).unwrap())
}
fn raw_request(state: &Path, bytes: &[u8]) -> Value {
    let mut s = UnixStream::connect(socket(state)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = s.write_all(bytes);
    let _ = s.shutdown(std::net::Shutdown::Write);
    let mut response = Vec::new();
    s.read_to_end(&mut response).unwrap();
    serde_json::from_slice(&response).unwrap()
}
fn temp() -> tempfile::TempDir {
    tempfile::tempdir_in(if cfg!(target_os = "macos") {
        Path::new("/private/tmp")
    } else {
        Path::new("/tmp")
    })
    .unwrap()
}
#[test]
fn locked_start_strict_protocol_and_namespace_binding() {
    let dir = temp();
    let mut child = start(dir.path());
    wait_ready(&mut child, dir.path());
    let status = request(dir.path(), "status");
    assert_eq!(status["locked"], true);
    assert!(status["exporter_pid"].is_null());
    assert!(status["broker_pid"].is_null());
    let mismatched = command(dir.path())
        .args(["service", "unlock"])
        .env("GENTLY_COLLECTOR_URL", "https://different.example")
        .env("GENTLY_TOKEN", "synthetic-ingest")
        .env("GENTLY_READ_TOKEN", "synthetic-read")
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(!mismatched.status.success());
    let diagnostic = String::from_utf8(mismatched.stderr).unwrap();
    assert!(diagnostic.contains("configuration mismatch"));
    assert!(!diagnostic.contains("synthetic"));
    assert_eq!(
        std::fs::metadata(socket(dir.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    for bad in [b"not json".as_slice(), br#"{"operation":"unlock","collector_url":"wrong","tenant":"test","device":"host","ingest":"synthetic-ingest","read":"synthetic-read"}"#] {
        let result = raw_request(dir.path(), bad);
        assert_eq!(result["ok"], false);
        assert_eq!(result["locked"], true);
        assert!(!result.to_string().contains("synthetic"));
    }
    assert_eq!(raw_request(dir.path(), &vec![b'x'; 17000])["ok"], false);
    assert!(!command(dir.path())
        .args(["service", "run"])
        .status()
        .unwrap()
        .success());
}

#[test]
fn separate_children_restart_lock_and_restart_locked() {
    let dir = temp();
    let mut child = start(dir.path());
    wait_ready(&mut child, dir.path());
    let unlock = command(dir.path())
        .args(["service", "unlock"])
        .env("GENTLY_TOKEN", "synthetic-ingest")
        .env("GENTLY_READ_TOKEN", "synthetic-read")
        .output()
        .unwrap();
    assert!(unlock.status.success(), "CLI unlock failed");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let s = request(dir.path(), "status");
        if s["exporter_pid"].is_u64() && s["broker_pid"].is_u64() {
            break s;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    };
    let exporter = status["exporter_pid"].as_u64().unwrap();
    let broker = status["broker_pid"].as_u64().unwrap();
    assert_ne!(exporter, broker);
    unsafe {
        libc::kill(exporter as i32, libc::SIGKILL);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = request(dir.path(), "status");
        if s["exporter_pid"]
            .as_u64()
            .is_some_and(|pid| pid != exporter)
        {
            break;
        }
        assert!(Instant::now() < deadline, "exporter was not restarted");
        std::thread::sleep(Duration::from_millis(50));
    }
    let locked = request(dir.path(), "lock");
    assert_eq!(locked["locked"], true);
    assert!(locked["exporter_pid"].is_null());
    assert!(locked["broker_pid"].is_null());
    assert_eq!(unsafe { libc::kill(broker as i32, 0) }, -1);
    assert_eq!(request(dir.path(), "unlock")["ok"], true);
    let deadline = Instant::now() + Duration::from_secs(10);
    let active = loop {
        let s = request(dir.path(), "status");
        if s["exporter_pid"].is_u64() && s["broker_pid"].is_u64() {
            break s;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(50));
    };
    drop(child);
    for key in ["exporter_pid", "broker_pid"] {
        assert_eq!(
            unsafe { libc::kill(active[key].as_u64().unwrap() as i32, 0) },
            -1,
            "child survived controller termination"
        );
    }
    let mut child = start(dir.path());
    wait_ready(&mut child, dir.path());
    assert_eq!(request(dir.path(), "status")["locked"], true);
}
