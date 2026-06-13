//! Local-only raw value capture and resolution.
//!
//! The collector sees only digest attributes. This module writes selected raw
//! hook values to the local SQLite store keyed by the same digest and, when
//! explicitly enabled, enriches local query results with those values.

use crate::query_client::SpanRow;
use anyhow::Result;
use gently_store::Store;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub const RESOLVE_ENV: &str = "GENTLY_RESOLVE_LOCAL_SHA_RAW_VALUES";

const JSON_RAW_FIELDS: &[&str] = &["tool_input", "tool_response"];
const TEXT_RAW_FIELDS: &[&str] = &[
    "prompt",
    "user",
    "user_prompt",
    "last_assistant_message",
    "assistant",
    "assistant_message",
];
const RESOLVABLE_ATTR_BASES: &[&str] = &[
    "gently.tool_input",
    "gently.tool_response",
    "gently.prompt",
    "gently.user",
    "gently.assistant",
];

pub fn resolve_enabled() -> bool {
    std::env::var_os(RESOLVE_ENV).is_some()
}

/// Capture selected raw hook values to the local SHA-to-string table.
pub fn capture_hook_values(store: &Store, raw: &Value) -> Result<()> {
    for field in JSON_RAW_FIELDS {
        if let Some(value) = raw.get(*field) {
            capture_json_value(store, value)?;
        }
    }
    for field in TEXT_RAW_FIELDS {
        if let Some(value) = raw.get(*field).and_then(Value::as_str) {
            capture_bytes(store, value.as_bytes(), value)?;
        }
    }
    Ok(())
}

/// Add local raw values to returned span attributes when their digest is known.
pub fn resolve_rows(store: &Store, rows: &mut [SpanRow]) -> Result<()> {
    for row in rows {
        resolve_attrs_json(store, &mut row.attrs_json)?;
    }
    Ok(())
}

fn capture_json_value(store: &Store, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let raw = String::from_utf8(bytes.clone()).expect("serde_json emits UTF-8");
    capture_bytes(store, &bytes, &raw)
}

fn capture_bytes(store: &Store, bytes: &[u8], raw: &str) -> Result<()> {
    store.raw_value_put(&digest_prefix(bytes), raw)?;
    Ok(())
}

fn digest_prefix(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn resolve_attrs_json(store: &Store, attrs_json: &mut Option<String>) -> Result<()> {
    let Some(blob) = attrs_json else {
        return Ok(());
    };
    let Ok(mut attrs) = serde_json::from_str::<Value>(blob) else {
        return Ok(());
    };
    let Some(arr) = attrs.as_array_mut() else {
        return Ok(());
    };

    let mut existing: HashSet<String> = arr
        .iter()
        .filter_map(attr_key)
        .map(str::to_string)
        .collect();
    let mut additions = Vec::new();

    for attr in arr.iter() {
        let Some(base) = attr_key(attr).and_then(raw_attr_base) else {
            continue;
        };
        if existing.contains(base) {
            continue;
        }
        let Some(sha) = attr_string_value(attr) else {
            continue;
        };
        if let Some(raw) = store.raw_value_get(sha)? {
            additions.push(json!({
                "key": base,
                "value": {"stringValue": raw},
            }));
            existing.insert(base.to_string());
        }
    }

    if !additions.is_empty() {
        arr.extend(additions);
        *blob = serde_json::to_string(&attrs)?;
    }
    Ok(())
}

fn raw_attr_base(key: &str) -> Option<&'static str> {
    let base = key.strip_suffix(".sha256")?;
    RESOLVABLE_ATTR_BASES
        .iter()
        .copied()
        .find(|candidate| *candidate == base)
}

fn attr_key(attr: &Value) -> Option<&str> {
    attr.get("key").and_then(Value::as_str)
}

fn attr_string_value(attr: &Value) -> Option<&str> {
    attr.get("value")?
        .get("stringValue")?
        .as_str()
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("state.db")).unwrap();
        (dir, s)
    }

    fn row(attrs: Value) -> SpanRow {
        SpanRow {
            span_id: "span".into(),
            trace_id: "trace".into(),
            parent_span_id: None,
            name: "Bash".into(),
            kind: 3,
            start_unix_nano: "1".into(),
            end_unix_nano: Some("2".into()),
            status: 1,
            session_id: Some("session".into()),
            harness: Some("codex".into()),
            tool_name: Some("Bash".into()),
            tool_use_id: Some("tu".into()),
            resource_json: None,
            attrs_json: Some(serde_json::to_string(&attrs).unwrap()),
            effective_end_unix_nano: None,
        }
    }

    #[test]
    fn capture_hook_values_stores_selected_raw_values() {
        let (_d, s) = store();
        capture_hook_values(
            &s,
            &json!({
                "prompt": "hello",
                "tool_input": {"command": "ls"},
                "tool_response": {"ok": true},
                "last_assistant_message": "done",
                "cwd": "/not/captured"
            }),
        )
        .unwrap();

        assert_eq!(
            s.raw_value_get(&digest_prefix(b"hello"))
                .unwrap()
                .as_deref(),
            Some("hello")
        );
        assert_eq!(
            s.raw_value_get(&digest_prefix(br#"{"command":"ls"}"#))
                .unwrap()
                .as_deref(),
            Some(r#"{"command":"ls"}"#)
        );
        assert_eq!(
            s.raw_value_get(&digest_prefix(b"/not/captured")).unwrap(),
            None
        );
    }

    #[test]
    fn resolve_rows_adds_raw_attr_from_local_sha_lookup() {
        let (_d, s) = store();
        let raw = r#"{"command":"ls"}"#;
        let sha = digest_prefix(raw.as_bytes());
        s.raw_value_put(&sha, raw).unwrap();
        let mut rows = vec![row(json!([
            {"key": "gently.tool_input.sha256", "value": {"stringValue": sha}},
            {"key": "gently.tool_input.bytes", "value": {"stringValue": raw.len().to_string()}}
        ]))];

        resolve_rows(&s, &mut rows).unwrap();

        let attrs: Value = serde_json::from_str(rows[0].attrs_json.as_deref().unwrap()).unwrap();
        assert!(attrs.as_array().unwrap().iter().any(|attr| {
            attr["key"] == "gently.tool_input" && attr["value"]["stringValue"] == raw
        }));
    }

    #[test]
    fn resolve_rows_does_not_overwrite_existing_raw_attr() {
        let (_d, s) = store();
        s.raw_value_put("abc", "resolved").unwrap();
        let mut rows = vec![row(json!([
            {"key": "gently.prompt.sha256", "value": {"stringValue": "abc"}},
            {"key": "gently.prompt", "value": {"stringValue": "existing"}}
        ]))];

        resolve_rows(&s, &mut rows).unwrap();

        let attrs: Value = serde_json::from_str(rows[0].attrs_json.as_deref().unwrap()).unwrap();
        let prompt_attrs = attrs
            .as_array()
            .unwrap()
            .iter()
            .filter(|attr| attr["key"] == "gently.prompt")
            .count();
        assert_eq!(prompt_attrs, 1);
        assert!(attrs.as_array().unwrap().iter().any(|attr| {
            attr["key"] == "gently.prompt" && attr["value"]["stringValue"] == "existing"
        }));
    }
}
