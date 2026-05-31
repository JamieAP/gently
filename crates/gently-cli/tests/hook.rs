//! Integration test for the `gently hook` contract: empty stdout, exit 0, and a
//! span lands in the outbox after a Pre/PostToolUse pair.

use assert_cmd::Command;
use gently_store::Store;

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

    // The PostToolUse closed the tool span -> exactly one OTLP row buffered.
    let store = Store::open(&state.join("state.db")).unwrap();
    assert_eq!(store.outbox_len().unwrap(), 1);
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
