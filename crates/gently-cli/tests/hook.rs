//! Synthetic integration coverage for the silent hook and ciphertext boundary.

use assert_cmd::Command;
use gently_raw::{DeviceIdentity, Manifest, OwnerKey, ReaderIdentities, Recipient, TrustPin};
use gently_store::Store;
use std::path::{Path, PathBuf};

fn runtime(state: &Path) -> PathBuf {
    state.join("tenants/personal/devices/capture-host")
}

fn run_hook(state_dir: &Path, payload: &str) {
    run_hook_capture(state_dir, payload, false);
}

fn run_hook_capture(state_dir: &Path, payload: &str, capture: bool) {
    Command::cargo_bin("gently")
        .unwrap()
        .arg("hook")
        .env("GENTLY_STATE_DIR", state_dir)
        .env("GENTLY_TENANT_ID", "personal")
        .env("GENTLY_DEVICE_ID", "capture-host")
        .env_remove("GENTLY_COLLECTOR_URL")
        .env_remove("GENTLY_TOKEN")
        .env("GENTLY_CAPTURE_RAW_VALUES", if capture { "1" } else { "0" })
        .env("GENTLY_RAW_MANIFEST", state_dir.join("manifest.json"))
        .env("GENTLY_RAW_TRUST", state_dir.join("trust.json"))
        .env_remove("GENTLY_RAW_IDENTITY")
        .write_stdin(payload)
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

fn enrollment(state: &Path) -> DeviceIdentity {
    enrollment_expiry(state, 4_102_444_800)
}

fn enrollment_expiry(state: &Path, expires_unix_secs: u64) -> DeviceIdentity {
    let identity = DeviceIdentity::generate();
    let owner = OwnerKey::generate();
    let manifest = Manifest {
        version: 1,
        tenant_id: "personal".into(),
        key_epoch: 1,
        expires_unix_secs,
        readers: vec![Recipient {
            device_id: "reader-host".into(),
            key_id: "reader-1".into(),
            recipient: identity.to_public().to_string(),
        }],
    };
    let signed = gently_raw::sign_manifest(manifest, &owner).unwrap();
    let pin = TrustPin {
        tenant_id: "personal".into(),
        owner_verify_key_b64: owner.verification_key_b64(),
        min_epoch: 1,
        manifest_digest: gently_raw::manifest_digest(&signed.manifest).unwrap(),
    };
    std::fs::write(
        state.join("manifest.json"),
        serde_json::to_vec(&signed).unwrap(),
    )
    .unwrap();
    std::fs::write(state.join("trust.json"), serde_json::to_vec(&pin).unwrap()).unwrap();
    identity
}

#[test]
fn hook_is_silent_exits_zero_and_buffers_length_only_spans() {
    let dir = tempfile::tempdir().unwrap();
    for payload in [
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"itest","cwd":"/w","prompt":"guessable fixture"}"#,
        r#"{"hook_event_name":"PreToolUse","session_id":"itest","cwd":"/w","tool_name":"Bash","tool_use_id":"tu_1","tool_input":{"command":"ls"}}"#,
        r#"{"hook_event_name":"PostToolUse","session_id":"itest","cwd":"/w","tool_name":"Bash","tool_use_id":"tu_1","tool_response":{"ok":true},"duration_ms":5}"#,
    ] {
        run_hook(dir.path(), payload);
    }
    let store = Store::open(&runtime(dir.path()).join("state.db")).unwrap();
    assert_eq!(store.outbox_len().unwrap(), 2);
    assert_eq!(store.raw_objects_len().unwrap(), 0);
    let queued = store.outbox_take_batch(10).unwrap();
    for (_, json) in queued {
        assert!(!json.contains(".sha256"));
        assert!(!json.contains(".raw_ref"));
        assert!(!json.contains("guessable fixture"));
    }
}

#[test]
fn raw_capture_encrypts_public_only_and_binds_open_tool_input_to_completed_span() {
    let dir = tempfile::tempdir().unwrap();
    let identity = enrollment(dir.path());
    for payload in [
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"raw-test","prompt":"private prompt fixture"}"#,
        r#"{"hook_event_name":"PreToolUse","session_id":"raw-test","tool_name":"Bash","tool_use_id":"tu_1","tool_input":{"command":"private command fixture"}}"#,
        r#"{"hook_event_name":"PostToolUse","session_id":"raw-test","tool_name":"Bash","tool_use_id":"tu_1","tool_response":{"answer":"private response fixture"}}"#,
    ] {
        run_hook_capture(dir.path(), payload, true);
    }
    let store = Store::open(&runtime(dir.path()).join("state.db")).unwrap();
    assert_eq!(store.raw_objects_len().unwrap(), 3);
    let objects = store.raw_objects_pending("personal", 10).unwrap();
    let identities = ReaderIdentities::from_native(vec![identity]);
    let mut fields = std::collections::BTreeMap::new();
    let tool_id = gently_core::SpanId::derive("raw-test", "tool:tu_1").to_hex();
    for object in objects {
        let payload = gently_raw::open(&object, &object.context, &identities).unwrap();
        if payload.fields.contains_key("gently.tool_input") {
            assert!(payload.bindings["gently.tool_input"].contains(&tool_id));
        }
        fields.extend(payload.fields);
    }
    assert_eq!(fields["gently.prompt"], "private prompt fixture");
    assert_eq!(
        fields["gently.tool_input"],
        r#"{"command":"private command fixture"}"#
    );
    assert_eq!(
        fields["gently.tool_response"],
        r#"{"answer":"private response fixture"}"#
    );
    let queued = store.outbox_take_batch(10).unwrap();
    let completed = queued
        .iter()
        .find(|(_, json)| json.contains("\"name\":\"Bash\""))
        .unwrap();
    assert!(completed.1.contains("gently.tool_input.raw_ref"));
    assert!(completed.1.contains("gently.tool_response.raw_ref"));
    assert!(!completed.1.contains("private command fixture"));
    assert!(!completed.1.contains(".sha256"));
    assert!(
        !runtime(dir.path()).join("export.log").exists(),
        "capture has no collector credential or private reader key"
    );
}

#[test]
fn repeated_content_has_unrelated_references_and_ciphertext() {
    let dir = tempfile::tempdir().unwrap();
    enrollment(dir.path());
    for _ in 0..2 {
        run_hook_capture(
            dir.path(),
            r#"{"hook_event_name":"UserPromptSubmit","session_id":"repeated","prompt":"same fixture"}"#,
            true,
        );
    }
    let store = Store::open(&runtime(dir.path()).join("state.db")).unwrap();
    let objects = store.raw_objects_pending("personal", 10).unwrap();
    assert_eq!(objects.len(), 2);
    assert_ne!(objects[0].context.raw_ref, objects[1].context.raw_ref);
    assert_ne!(objects[0].ciphertext_b64, objects[1].ciphertext_b64);
}

#[test]
fn encrypted_capture_and_debug_never_persist_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    enrollment(dir.path());
    Command::cargo_bin("gently").unwrap().arg("hook")
        .env("GENTLY_STATE_DIR", dir.path()).env("GENTLY_DEBUG", "1")
        .env("GENTLY_TENANT_ID", "personal").env("GENTLY_DEVICE_ID", "capture-host")
        .env("GENTLY_CAPTURE_RAW_VALUES", "1")
        .env("GENTLY_RAW_MANIFEST", dir.path().join("manifest.json"))
        .env("GENTLY_RAW_TRUST", dir.path().join("trust.json"))
        .env_remove("GENTLY_COLLECTOR_URL").env_remove("GENTLY_TOKEN").env_remove("GENTLY_RAW_IDENTITY")
        .write_stdin(r#"{"hook_event_name":"UserPromptSubmit","session_id":"privacy-test","prompt":"plaintext-canary-unique-fixture"}"#)
        .assert().success().stdout(predicates::str::is_empty());
    assert!(!dir.path().join("raw").exists());
    let store = Store::open(&runtime(dir.path()).join("state.db")).unwrap();
    assert_eq!(store.raw_objects_len().unwrap(), 1);
    for entry in std::fs::read_dir(runtime(dir.path())).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let bytes = std::fs::read(path).unwrap();
            let canary = b"plaintext-canary-unique-fixture";
            assert!(!bytes.windows(canary.len()).any(|w| w == canary));
        }
    }
}

#[test]
fn untrusted_recipient_policy_fails_capture_without_fallback_or_payload_log() {
    let dir = tempfile::tempdir().unwrap();
    enrollment(dir.path());
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("manifest.json")).unwrap()).unwrap();
    manifest["manifest"]["readers"][0]["recipient"] =
        DeviceIdentity::generate().to_public().to_string().into();
    std::fs::write(
        dir.path().join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    run_hook_capture(
        dir.path(),
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"policy-test","prompt":"private invalid-policy canary"}"#,
        true,
    );
    let store = Store::open(&runtime(dir.path()).join("state.db")).unwrap();
    assert_eq!(store.raw_objects_len().unwrap(), 0);
    assert_eq!(store.outbox_len().unwrap(), 1);
    assert_eq!(store.open_span_attributes().unwrap().len(), 1);
    assert!(!store.outbox_take_batch(10).unwrap()[0]
        .1
        .contains(".raw_ref"));
    let log = std::fs::read_to_string(runtime(dir.path()).join("hook.log")).unwrap();
    assert!(!log.contains("private invalid-policy canary"));
    assert!(!dir.path().join("raw").exists());
}

#[test]
fn missing_recipient_policy_fails_capture_silently() {
    let dir = tempfile::tempdir().unwrap();
    run_hook_capture(
        dir.path(),
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"missing-policy","prompt":"fixture"}"#,
        true,
    );
    let store = Store::open(&runtime(dir.path()).join("state.db")).unwrap();
    assert_eq!(store.raw_objects_len().unwrap(), 0);
    assert_eq!(store.outbox_len().unwrap(), 1);
    assert!(!store.outbox_take_batch(10).unwrap()[0]
        .1
        .contains(".raw_ref"));
}

#[test]
fn expired_policy_preserves_metadata_and_never_creates_raw_references() {
    let dir = tempfile::tempdir().unwrap();
    enrollment_expiry(dir.path(), 1);
    run_hook_capture(
        dir.path(),
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"expired","prompt":"expired policy private fixture"}"#,
        true,
    );
    let store = Store::open(&runtime(dir.path()).join("state.db")).unwrap();
    assert_eq!(store.raw_objects_len().unwrap(), 0);
    let queued = store.outbox_take_batch(10).unwrap();
    assert_eq!(queued.len(), 1);
    assert!(queued[0].1.contains("gently.prompt.bytes"));
    assert!(!queued[0].1.contains(".raw_ref"));
    assert!(!queued[0].1.contains("expired policy private fixture"));
}

#[test]
fn capture_failure_preserves_authenticated_refs_inherited_from_open_tool() {
    let dir = tempfile::tempdir().unwrap();
    let identity = enrollment(dir.path());
    run_hook_capture(
        dir.path(),
        r#"{"hook_event_name":"UserPromptSubmit","session_id":"inherited","prompt":"fixture"}"#,
        true,
    );
    run_hook_capture(
        dir.path(),
        r#"{"hook_event_name":"PreToolUse","session_id":"inherited","tool_name":"Bash","tool_use_id":"tool_1","tool_input":{"command":"private input fixture"}}"#,
        true,
    );
    let store = Store::open(&runtime(dir.path()).join("state.db")).unwrap();
    let input = store
        .raw_objects_pending("personal", 10)
        .unwrap()
        .into_iter()
        .find(|object| object.context.event == "PreToolUse")
        .unwrap();
    std::fs::remove_file(dir.path().join("manifest.json")).unwrap();
    run_hook_capture(
        dir.path(),
        r#"{"hook_event_name":"PostToolUse","session_id":"inherited","tool_name":"Bash","tool_use_id":"tool_1","tool_response":{"answer":"private output fixture"}}"#,
        true,
    );
    assert_eq!(store.raw_objects_len().unwrap(), 2);
    let queued = store.outbox_take_batch(10).unwrap();
    let completed = queued
        .iter()
        .find(|(_, json)| json.contains("\"name\":\"Bash\""))
        .unwrap();
    assert!(completed.1.contains(&input.context.raw_ref));
    assert!(completed.1.contains("gently.tool_input.raw_ref"));
    assert!(completed.1.contains("gently.tool_response.bytes"));
    assert!(!completed.1.contains("gently.tool_response.raw_ref"));
    assert!(!completed.1.contains("private input fixture"));
    assert!(!completed.1.contains("private output fixture"));
    let payload = gently_raw::open(
        &input,
        &input.context,
        &ReaderIdentities::from_native(vec![identity]),
    )
    .unwrap();
    assert_eq!(
        payload.fields["gently.tool_input"],
        r#"{"command":"private input fixture"}"#
    );
    assert!(payload.bindings["gently.tool_input"]
        .contains(&gently_core::SpanId::derive("inherited", "tool:tool_1").to_hex()));
}

#[test]
fn oversized_raw_payload_rolls_back_refs_but_preserves_metadata() {
    let dir = tempfile::tempdir().unwrap();
    enrollment(dir.path());
    let payload = serde_json::json!({"hook_event_name":"UserPromptSubmit","session_id":"large-fixture","prompt":"large-private-fixture".repeat(20_000)}).to_string();
    run_hook_capture(dir.path(), &payload, true);
    let store = Store::open(&runtime(dir.path()).join("state.db")).unwrap();
    assert_eq!(store.raw_objects_len().unwrap(), 0);
    let queued = store.outbox_take_batch(10).unwrap();
    assert_eq!(queued.len(), 1);
    assert!(queued[0].1.contains("gently.prompt.bytes"));
    assert!(!queued[0].1.contains(".raw_ref"));
    assert!(!queued[0].1.contains("large-private-fixture"));
    let pending = store.open_span_attributes().unwrap();
    assert_eq!(pending.len(), 1);
    assert!(!pending[0].1.contains(".raw_ref"));
}

#[test]
fn malformed_payload_still_exits_zero_and_silent() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("gently")
        .unwrap()
        .arg("hook")
        .env("GENTLY_STATE_DIR", dir.path())
        .env_remove("GENTLY_TOKEN")
        .write_stdin("not json at all")
        .assert()
        .success()
        .stdout(predicates::str::is_empty());
}

#[test]
fn multi_span_event_uses_one_envelope() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("gently").unwrap().arg("hook").arg("--harness").arg("codex")
        .env("GENTLY_STATE_DIR", dir.path()).env_remove("GENTLY_TOKEN").env("GENTLY_CAPTURE_RAW_VALUES", "0")
        .env("GENTLY_TENANT_ID", "personal").env("GENTLY_DEVICE_ID", "capture-host")
        .write_stdin(r#"{"hook_event_name":"PostToolUse","session_id":"missed-pre","turn_id":"t","tool_name":"Bash","tool_use_id":"u","tool_response":"opaque"}"#)
        .assert().success().stdout(predicates::str::is_empty());
    let store = Store::open(&runtime(dir.path()).join("state.db")).unwrap();
    assert_eq!(store.outbox_len().unwrap(), 1);
    let rows = store.outbox_take_batch(10).unwrap();
    let req: serde_json::Value = serde_json::from_str(&rows[0].1).unwrap();
    assert_eq!(
        req["resourceSpans"][0]["scopeSpans"][0]["spans"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(!runtime(dir.path()).join("export.log").exists());
}
