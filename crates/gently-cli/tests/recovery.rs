//! Disposable synthetic state only; never unlocks a reader or opens live settings.
use assert_cmd::Command;
use gently_store::Store;
use serde_json::{json, Value};
use std::path::Path;

fn command(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("gently").unwrap();
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("GENTLY_STATE_DIR", home.join(".gently"))
        .env("GENTLY_TENANT_ID", "synthetic-tenant")
        .env("GENTLY_DEVICE_ID", "synthetic-device");
    cmd
}
fn json_file(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn state_backup_restores_live_queue_and_refuses_existing_state_or_wrong_namespace() {
    let home = tempfile::tempdir().unwrap();
    command(home.path())
        .args(["init", "--claude"])
        .assert()
        .success();
    let database = home
        .path()
        .join(".gently/tenants/synthetic-tenant/devices/synthetic-device/state.db");
    std::fs::create_dir_all(database.parent().unwrap()).unwrap();
    let store = Store::open(&database).unwrap();
    store.outbox_enqueue("synthetic metadata").unwrap();
    let backup = home.path().join("backup.db");
    command(home.path())
        .args(["state", "backup"])
        .arg(&backup)
        .assert()
        .success();
    command(home.path())
        .args(["state", "restore"])
        .arg(&backup)
        .assert()
        .failure();
    assert_eq!(store.outbox_len().unwrap(), 1);
    drop(store);
    std::fs::remove_file(&database).unwrap();
    command(home.path())
        .env("GENTLY_DEVICE_ID", "other-device")
        .args(["state", "restore"])
        .arg(&backup)
        .assert()
        .failure();
    command(home.path())
        .args(["state", "restore"])
        .arg(&backup)
        .assert()
        .success();
    assert_eq!(Store::open(&database).unwrap().outbox_len().unwrap(), 1);
}

#[test]
fn claude_uninstall_preserves_user_hooks_preferences_and_state_and_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    command(home.path())
        .args(["init", "--claude"])
        .assert()
        .success();
    let settings = home.path().join(".claude/settings.json");
    let mut config = json_file(&settings);
    let user = json!({"matcher":"user-only","hooks":[{"type":"command","command":"user-check"}]});
    config["hooks"]["PreToolUse"]
        .as_array_mut()
        .unwrap()
        .push(user.clone());
    config["syntheticUserPreference"] = json!({"kept":true});
    std::fs::write(&settings, serde_json::to_vec(&config).unwrap()).unwrap();
    let mcp = home.path().join(".claude.json");
    let mut config = json_file(&mcp);
    config["mcpServers"]["user"] = json!({"command":"user-mcp"});
    std::fs::write(&mcp, serde_json::to_vec(&config).unwrap()).unwrap();
    let state = home.path().join(".gently/config.toml");
    let retained = std::fs::read(&state).unwrap();
    command(home.path())
        .args(["uninstall", "--claude"])
        .assert()
        .success();
    assert_eq!(json_file(&settings)["hooks"]["PreToolUse"], json!([user]));
    assert_eq!(
        json_file(&settings)["syntheticUserPreference"]["kept"],
        true
    );
    assert!(json_file(&mcp)["mcpServers"].get("gently").is_none());
    assert_eq!(json_file(&mcp)["mcpServers"]["user"]["command"], "user-mcp");
    assert_eq!(std::fs::read(&state).unwrap(), retained);
    let first = std::fs::read(&settings).unwrap();
    command(home.path())
        .args(["uninstall", "--claude"])
        .assert()
        .success();
    assert_eq!(std::fs::read(&settings).unwrap(), first);
}

#[test]
fn codex_uninstall_preserves_user_preferences_and_custom_executable_paths() {
    let home = tempfile::tempdir().unwrap();
    command(home.path())
        .args(["init", "--codex"])
        .assert()
        .success();
    let path = home.path().join(".codex/config.toml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n[[hooks.PreToolUse]]\nmatcher = 'user-only'\n[[hooks.PreToolUse.hooks]]\ntype = 'command'\ncommand = 'custom-gently hook'\n\n[mcp_servers.user]\ncommand = 'user-mcp'\n");
    std::fs::write(&path, text).unwrap();
    command(home.path())
        .args(["uninstall", "--codex"])
        .assert()
        .success();
    let text = std::fs::read_to_string(&path).unwrap();
    let doc: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(doc["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str(),
        Some("custom-gently hook")
    );
    assert!(doc["mcp_servers"].get("gently").is_none());
    assert_eq!(
        doc["mcp_servers"]["user"]["command"].as_str(),
        Some("user-mcp")
    );
    assert_eq!(doc["features"]["hooks"].as_bool(), Some(true));
    command(home.path())
        .args(["uninstall", "--codex"])
        .assert()
        .success();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    // A server using another executable path belongs to the user.
    std::fs::write(&path,"# retain formatting\nmcp_servers = { gently = { command = 'custom-gently', args = ['mcp'] } }\n").unwrap();
    let before = std::fs::read(&path).unwrap();
    command(home.path())
        .args(["uninstall", "--codex"])
        .assert()
        .success();
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn uninstall_validates_all_selected_files_before_editing_any() {
    for harness in ["--codex", "--claude"] {
        let home = tempfile::tempdir().unwrap();
        command(home.path())
            .args(["init", harness])
            .assert()
            .success();
        let (valid, bad) = match harness {
            "--codex" => (
                home.path().join(".codex/config.toml"),
                home.path().join(".codex/hooks.json"),
            ),
            _ => (
                home.path().join(".claude/settings.json"),
                home.path().join(".claude.json"),
            ),
        };
        let before = std::fs::read(&valid).unwrap();
        let invalid = b"{\"synthetic-parse-canary\":";
        std::fs::write(&bad, invalid).unwrap();
        let result = command(home.path())
            .args(["uninstall", harness])
            .assert()
            .failure()
            .get_output()
            .clone();
        assert_eq!(std::fs::read(&valid).unwrap(), before);
        assert_eq!(std::fs::read(&bad).unwrap(), invalid);
        assert!(!String::from_utf8_lossy(&result.stderr).contains("synthetic-parse-canary"));
    }
}

#[test]
fn uninstall_retains_platform_overrides_on_an_exact_managed_command() {
    for harness in ["--claude", "--codex"] {
        let home = tempfile::tempdir().unwrap();
        command(home.path())
            .args(["init", harness])
            .assert()
            .success();
        if harness == "--claude" {
            let path = home.path().join(".claude/settings.json");
            let mut config = json_file(&path);
            config["hooks"]["PreToolUse"][0]["hooks"][0]["commandWindows"] =
                json!("user-windows-command");
            let retained = config["hooks"]["PreToolUse"].clone();
            std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
            command(home.path())
                .args(["uninstall", harness])
                .assert()
                .success();
            assert_eq!(json_file(&path)["hooks"]["PreToolUse"], retained);
        } else {
            let path = home.path().join(".codex/config.toml");
            let mut doc: toml_edit::DocumentMut =
                std::fs::read_to_string(&path).unwrap().parse().unwrap();
            doc["hooks"]["PreToolUse"]
                .as_array_of_tables_mut()
                .unwrap()
                .get_mut(0)
                .unwrap()["hooks"]
                .as_array_of_tables_mut()
                .unwrap()
                .get_mut(0)
                .unwrap()["command_windows"] = toml_edit::value("user-windows-command");
            std::fs::write(&path, doc.to_string()).unwrap();
            command(home.path())
                .args(["uninstall", harness])
                .assert()
                .success();
            let doc: toml::Value =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(
                doc["hooks"]["PreToolUse"][0]["hooks"][0]["command_windows"].as_str(),
                Some("user-windows-command")
            );
        }
    }
}

#[test]
fn uninstall_refuses_invalid_selected_json_shapes_before_other_edits() {
    for harness in ["--codex", "--claude"] {
        for invalid in [
            json!([]),
            json!("invalid-root"),
            json!({"hooks":{"PreToolUse":["invalid-group"]}}),
            json!({"hooks":{"PreToolUse":[{"hooks":["invalid-handler"]}]}}),
        ] {
            let home = tempfile::tempdir().unwrap();
            command(home.path())
                .args(["init", harness])
                .assert()
                .success();
            let (bad, valid) = match harness {
                "--codex" => (
                    home.path().join(".codex/hooks.json"),
                    home.path().join(".codex/config.toml"),
                ),
                _ => (
                    home.path().join(".claude/settings.json"),
                    home.path().join(".claude.json"),
                ),
            };
            std::fs::write(&bad, serde_json::to_vec(&invalid).unwrap()).unwrap();
            let before = std::fs::read(&valid).unwrap();
            command(home.path())
                .args(["uninstall", harness])
                .assert()
                .failure();
            assert_eq!(std::fs::read(&valid).unwrap(), before);
        }
    }
}

#[test]
fn malformed_selected_mcp_entries_preserve_installed_hooks() {
    for harness in ["--codex", "--claude"] {
        let home = tempfile::tempdir().unwrap();
        command(home.path())
            .args(["init", harness])
            .assert()
            .success();
        let hooks = home.path().join(if harness == "--codex" {
            ".codex/config.toml"
        } else {
            ".claude/settings.json"
        });
        if harness == "--codex" {
            let mut doc: toml_edit::DocumentMut =
                std::fs::read_to_string(&hooks).unwrap().parse().unwrap();
            doc["mcp_servers"]["invalid"] = toml_edit::value("synthetic-invalid");
            let contents = doc.to_string();
            let _: toml::Value = toml::from_str(&contents).unwrap();
            std::fs::write(&hooks, contents).unwrap();
        } else {
            std::fs::write(
                home.path().join(".claude.json"),
                br#"{"mcpServers":{"invalid":"synthetic-invalid"}}"#,
            )
            .unwrap();
        }
        let before = std::fs::read(&hooks).unwrap();
        command(home.path())
            .args(["uninstall", harness])
            .assert()
            .failure();
        assert_eq!(std::fs::read(&hooks).unwrap(), before);
    }
}
