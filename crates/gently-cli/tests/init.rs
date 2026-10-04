//! Integration test for `gently init --claude`: installs hooks + MCP server +
//! config into a scratch HOME, and is idempotent.

use assert_cmd::Command;
use serde_json::Value;

fn init(home: &std::path::Path) {
    Command::cargo_bin("gently")
        .unwrap()
        .arg("init")
        .arg("--claude")
        .env("HOME", home)
        .env("GENTLY_STATE_DIR", home.join(".gently"))
        .assert()
        .success();
}

#[test]
fn init_installs_hooks_mcp_and_config_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();

    init(home);

    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap())
            .unwrap();
    let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 1, "one PreToolUse hook installed");
    assert!(pre[0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .ends_with(" hook"));

    let claude_json: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
    assert_eq!(claude_json["mcpServers"]["gently"]["args"][0], "mcp");
    assert!(claude_json["mcpServers"]["gently"]["env"]["GENTLY_RESOLVE_LOCAL_SHA_RAW_VALUES"].is_null(),
        "ordinary installation must not expose raw prompt/tool values over MCP");

    assert!(home.join(".gently/config.toml").exists());

    // Running again must not duplicate the hook entry.
    init(home);
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        settings["hooks"]["PreToolUse"].as_array().unwrap().len(),
        1,
        "init is idempotent"
    );
}

#[test]
fn raw_mcp_resolution_requires_an_explicit_install_option() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("gently").unwrap()
        .args(["init", "--claude", "--resolve-local-raw-values"])
        .env("HOME", dir.path())
        .env("GENTLY_STATE_DIR", dir.path().join(".gently"))
        .assert().success();
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(".claude.json")).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["gently"]["env"]["GENTLY_RESOLVE_LOCAL_SHA_RAW_VALUES"], "1");
    init(dir.path());
    let config: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(".claude.json")).unwrap()).unwrap();
    assert!(config["mcpServers"]["gently"]["env"]["GENTLY_RESOLVE_LOCAL_SHA_RAW_VALUES"].is_null());
}

#[cfg(unix)]
#[test]
fn installation_protects_existing_state_and_config_files() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join(".gently");
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = state.join("config.toml");
    std::fs::write(&config, "token = 'fixture-token'\n").unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).unwrap();
    init(dir.path());
    assert_eq!(std::fs::metadata(state).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(std::fs::metadata(config).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(dir.path().join(".claude.json")).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn codex_install_raw_resolution_is_explicit_and_reversible() {
    let dir = tempfile::tempdir().unwrap();
    let run = |resolve: bool| {
        let mut command = Command::cargo_bin("gently").unwrap();
        command.args(["init", "--codex"]);
        if resolve { command.arg("--resolve-local-raw-values"); }
        command.env("HOME", dir.path()).env("GENTLY_STATE_DIR", dir.path().join(".gently"))
            .assert().success();
        let text = std::fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap();
        toml::from_str::<toml::Value>(&text).unwrap()
    };
    let initial = run(false);
    assert!(initial["mcp_servers"]["gently"].get("env").is_none());
    let opted_in = run(true);
    assert_eq!(opted_in["mcp_servers"]["gently"]["env"]["GENTLY_RESOLVE_LOCAL_SHA_RAW_VALUES"].as_str(), Some("1"));
    let reset = run(false);
    assert!(reset["mcp_servers"]["gently"].get("env").is_none());
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dir.path().join(".codex")).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(dir.path().join(".codex/config.toml")).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
