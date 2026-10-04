//! Integration test for the `gently hook` contract: empty stdout, exit 0, and a
//! span lands in the outbox after a Pre/PostToolUse pair.

use assert_cmd::Command;
use gently_store::Store;
use sha2::{Digest, Sha256};

fn run_hook(state_dir: &std::path::Path, payload: &str) {
    let mut cmd = Command::cargo_bin("gently").unwrap();
    cmd.arg("hook")
        .env("GENTLY_STATE_DIR", state_dir)
        // No collector configured: the detached export child is harmless here.
        .env_remove("GENTLY_COLLECTOR_URL")
        .env_remove("GENTLY_TOKEN")
        .write_stdin(payload);
    let out = cmd.assert().success();
    // Hard contract: hooks must never write to stdout.
    out.stdout(predicates::str::is_empty());
}

#[test]
fn hook_is_silent_exits_zero_and_buffers_a_span() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();

    run_hook(
        state,
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"itest","cwd":"/w","prompt":"hi"}"#,
    );
    run_hook(
        state,
        r#"{"hook_event_name":"PreToolUse","session_id":"itest","cwd":"/w","tool_name":"Bash","tool_use_id":"tu_1","tool_input":{"command":"ls"}}"#,
    );
    run_hook(
        state,
        r#"{"hook_event_name":"PostToolUse","session_id":"itest","cwd":"/w","tool_name":"Bash","tool_use_id":"tu_1","tool_response":{"ok":true},"duration_ms":5}"#,
    );

    // Two OTLP rows buffered: the provisional turn span (emitted on
    // UserPromptSubmit) and the completed Bash tool span (emitted on PostToolUse).
    let store = Store::open(&state.join("state.db")).unwrap();
    assert_eq!(store.outbox_len().unwrap(), 2);
    assert_eq!(store.raw_values_len().unwrap(), 3);
    assert_eq!(
        store.raw_value_get(&sha(b"hi")).unwrap().as_deref(),
        Some("hi")
    );
    assert_eq!(
        store
            .raw_value_get(&sha(br#"{"command":"ls"}"#))
            .unwrap()
            .as_deref(),
        Some(r#"{"command":"ls"}"#)
    );
    assert_eq!(
        store
            .raw_value_get(&sha(br#"{"ok":true}"#))
            .unwrap()
            .as_deref(),
        Some(r#"{"ok":true}"#)
    );

    let queued = store.outbox_take_batch(10).unwrap();
    let joined = queued
        .iter()
        .map(|(_, span_json)| span_json.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!joined.contains("hi"));
    assert!(!joined.contains(r#"{"command":"ls"}"#));
    assert!(!joined.contains(r#"{"ok":true}"#));
}

#[test]
fn malformed_payload_still_exits_zero_and_silent() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("gently").unwrap();
    cmd.arg("hook")
        .env("GENTLY_STATE_DIR", dir.path())
        .write_stdin("not json at all");
    cmd.assert().success().stdout(predicates::str::is_empty());
}

fn sha(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[cfg(unix)]
#[test]
fn debug_capture_protects_existing_payload_and_environment_files() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("raw/claude");
    std::fs::create_dir_all(&raw).unwrap();
    for p in [dir.path().join("raw"), raw.clone()] {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    for p in [raw.join("UserPromptSubmit.jsonl"), raw.join("env.json")] {
        std::fs::write(&p, "{}").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    Command::cargo_bin("gently").unwrap()
        .arg("hook").env("GENTLY_STATE_DIR", dir.path()).env("GENTLY_DEBUG", "1")
        .env_remove("GENTLY_COLLECTOR_URL").env_remove("GENTLY_TOKEN")
        .write_stdin(r#"{"hook_event_name":"UserPromptSubmit","session_id":"privacy-test","prompt":"private fixture"}"#)
        .assert().success();
    for p in [dir.path().join("raw"), raw.clone()] {
        assert_eq!(std::fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o700);
    }
    for p in [raw.join("UserPromptSubmit.jsonl"), raw.join("env.json")] {
        assert_eq!(std::fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
