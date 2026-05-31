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
