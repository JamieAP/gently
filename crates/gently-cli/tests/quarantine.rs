use assert_cmd::Command;
use gently_store::{QuarantineReason, Store};

const PAYLOAD_CANARY: &str = "synthetic-private-envelope-canary";

/// A local namespace with one invalid-JSON row and one collector-rejected row,
/// both carrying a canary payload that must never reach stdout.
fn namespace() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir
        .path()
        .join("tenants/personal/devices/synthetic-host/state.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let store = Store::open(&db).unwrap();
    for reason in [
        QuarantineReason::InvalidJson,
        QuarantineReason::CollectorRejection { status: 413 },
    ] {
        store.outbox_enqueue(PAYLOAD_CANARY).unwrap();
        let (id, _) = store.outbox_take_batch(1).unwrap()[0].clone();
        store.outbox_quarantine(&[id], reason).unwrap();
    }
    (dir, store)
}

fn gently(dir: &tempfile::TempDir, args: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("gently")
        .unwrap()
        .args(args)
        .env("GENTLY_STATE_DIR", dir.path())
        .env("GENTLY_TENANT_ID", "personal")
        .env("GENTLY_DEVICE_ID", "synthetic-host")
        .env_remove("GENTLY_COLLECTOR_URL")
        .env_remove("GENTLY_TOKEN")
        .assert()
}

fn stdout(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8(assert.get_output().stdout.clone()).unwrap()
}

#[test]
fn list_prints_typed_payload_free_json_summaries() {
    let (dir, _store) = namespace();
    let listed = gently(&dir, &["quarantine", "list"]).success();
    let out = stdout(&listed);
    assert!(!out.contains("canary"), "payload must never be printed");
    assert!(
        !out.contains("collector rejected"),
        "reason text stays local"
    );
    let rows: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        let mut keys: Vec<_> = row.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "bytes",
                "category",
                "http_status",
                "id",
                "quarantined_unix_nano"
            ]
        );
        assert_eq!(row["bytes"], PAYLOAD_CANARY.len());
    }
    assert_eq!(rows[0]["category"], "invalid_json");
    assert_eq!(rows[0]["http_status"], serde_json::Value::Null);
    assert_eq!(rows[1]["category"], "collector_rejection");
    assert_eq!(rows[1]["http_status"], 413);

    let first = rows[0]["id"].as_i64().unwrap().to_string();
    let page = gently(
        &dir,
        &["quarantine", "list", "--after-id", &first, "--limit", "1"],
    )
    .success();
    let page: Vec<serde_json::Value> = serde_json::from_str(&stdout(&page)).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0]["id"], rows[1]["id"]);
}

#[test]
fn retry_moves_exactly_one_row_and_rejects_an_absent_id() {
    let (dir, store) = namespace();
    let id = store.quarantine_summaries(0, 1).unwrap()[0].id.to_string();
    let retried = gently(&dir, &["quarantine", "retry", "--id", &id]).success();
    assert!(!stdout(&retried).contains("canary"));
    assert_eq!(store.outbox_len().unwrap(), 1);
    assert_eq!(store.quarantine_len().unwrap(), 1);

    let again = gently(&dir, &["quarantine", "retry", "--id", &id])
        .failure()
        .code(1)
        .stderr(predicates::str::contains("quarantine row not found"));
    assert!(stdout(&again).is_empty());
    gently(&dir, &["quarantine", "retry", "--id", "999"])
        .failure()
        .code(1);
    assert_eq!(store.outbox_len().unwrap(), 1);
    assert_eq!(store.quarantine_len().unwrap(), 1);
}
