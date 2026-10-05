//! Integration test for `gently init --claude`: installs hooks + MCP server +
//! config into a scratch HOME, and is idempotent.

use assert_cmd::Command;
use serde_json::Value;

fn isolated_init(home: &std::path::Path) -> Command {
    let mut command = Command::cargo_bin("gently").unwrap();
    command
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("GENTLY_STATE_DIR", home.join(".gently"));
    command
}

#[test]
fn codex_inline_tables_install_without_panics_and_preserve_preferences() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".codex")).unwrap();
    let path = dir.path().join(".codex/config.toml");
    std::fs::write(&path, "# inline preferences\nhooks = { PreToolUse = [{ hooks = [{ type = 'command', command = 'user-check' }] }] }\nmcp_servers = { other = { command = 'other-mcp' }, gently = { enabled = false, env = { USER_SETTING = 'kept' } } }\nfeatures = { multi_agent = true }\n").unwrap();
    isolated_init(dir.path())
        .args(["init", "--codex", "--resolve-raw-values"])
        .assert()
        .success();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# inline preferences"));
    let doc: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(doc["features"]["multi_agent"].as_bool(), Some(true));
    assert_eq!(doc["features"]["hooks"].as_bool(), Some(true));
    assert_eq!(
        doc["mcp_servers"]["other"]["command"].as_str(),
        Some("other-mcp")
    );
    assert_eq!(
        doc["mcp_servers"]["gently"]["enabled"].as_bool(),
        Some(false)
    );
    assert_eq!(
        doc["mcp_servers"]["gently"]["env"]["USER_SETTING"].as_str(),
        Some("kept")
    );
    assert_eq!(doc["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
    isolated_init(dir.path())
        .args(["init", "--codex"])
        .assert()
        .success();
    let text = std::fs::read_to_string(path).unwrap();
    let refreshed: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(
        refreshed["hooks"]["PreToolUse"].as_array().unwrap().len(),
        2
    );
    assert!(refreshed["mcp_servers"]["gently"]["env"]
        .get("GENTLY_RESOLVE_RAW_VALUES")
        .is_none());
}

#[test]
fn invalid_codex_shapes_return_clean_errors_and_preserve_existing_files() {
    for (field, contents) in [
        ("hooks", "hooks = 'unexpected'\n"),
        ("hooks.PreToolUse", "[hooks]\nPreToolUse = 'unexpected'\n"),
        (
            "hooks.PreToolUse.hooks",
            "[[hooks.PreToolUse]]\nhooks = 'unexpected'\n",
        ),
        ("mcp_servers", "mcp_servers = 'unexpected'\n"),
        (
            "mcp_servers.gently",
            "[mcp_servers]\ngently = 'unexpected'\n",
        ),
        (
            "mcp_servers.gently.env",
            "[mcp_servers.gently]\nenv = 'unexpected'\n",
        ),
        ("features", "features = 'unexpected'\n"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".codex")).unwrap();
        let path = dir.path().join(".codex/config.toml");
        std::fs::write(&path, contents).unwrap();
        let result = isolated_init(dir.path())
            .args(["init", "--codex"])
            .assert()
            .code(1)
            .get_output()
            .clone();
        let stderr = String::from_utf8(result.stderr).unwrap();
        assert!(stderr.contains(field), "{stderr}");
        assert!(!stderr.contains("panicked"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), contents);
    }
}

#[test]
fn codex_empty_inline_hook_arrays_accept_registration() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".codex")).unwrap();
    let path = dir.path().join(".codex/config.toml");
    std::fs::write(
        &path,
        "hooks = { PreToolUse = [], PostToolUse = [{ hooks = [] }] }\n",
    )
    .unwrap();
    isolated_init(dir.path())
        .args(["init", "--codex"])
        .assert()
        .success();
}

#[test]
fn unreadable_configuration_encoding_is_rejected_without_overwriting() {
    for (harness, name) in [
        ("--codex", ".codex/config.toml"),
        ("--claude", ".claude/settings.json"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let contents = b"synthetic-unreadable-config\xff";
        std::fs::write(&path, contents).unwrap();
        isolated_init(dir.path())
            .args(["init", harness])
            .assert()
            .failure();
        assert_eq!(std::fs::read(path).unwrap(), contents);
    }
}

#[test]
fn malformed_codex_toml_errors_never_echo_source_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".codex/config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let contents = "setting = 'synthetic-init-parse-canary\n";
    std::fs::write(&path, contents).unwrap();
    let result = isolated_init(dir.path())
        .args(["init", "--codex"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(!String::from_utf8_lossy(&result.stderr).contains("synthetic-init-parse-canary"));
    assert_eq!(std::fs::read_to_string(path).unwrap(), contents);
}

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
    assert!(
        claude_json["mcpServers"]["gently"]["env"]["GENTLY_RESOLVE_RAW_VALUES"].is_null(),
        "ordinary installation must not expose raw prompt/tool values over MCP"
    );

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
    Command::cargo_bin("gently")
        .unwrap()
        .args(["init", "--claude", "--resolve-raw-values"])
        .env("HOME", dir.path())
        .env("GENTLY_STATE_DIR", dir.path().join(".gently"))
        .assert()
        .success();
    let config: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join(".claude.json")).unwrap())
            .unwrap();
    assert_eq!(
        config["mcpServers"]["gently"]["env"]["GENTLY_RESOLVE_RAW_VALUES"],
        "1"
    );
    init(dir.path());
    let config: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join(".claude.json")).unwrap())
            .unwrap();
    assert!(config["mcpServers"]["gently"]["env"]["GENTLY_RESOLVE_RAW_VALUES"].is_null());
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
    std::fs::write(&config, "tenant_id = 'personal'\n").unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).unwrap();
    init(dir.path());
    assert_eq!(
        std::fs::metadata(state).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(config).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(dir.path().join(".claude.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn codex_install_raw_resolution_is_explicit_and_reversible() {
    let dir = tempfile::tempdir().unwrap();
    let run = |resolve: bool| {
        let mut command = Command::cargo_bin("gently").unwrap();
        command.args(["init", "--codex"]);
        if resolve {
            command.arg("--resolve-raw-values");
        }
        command
            .env("HOME", dir.path())
            .env("GENTLY_STATE_DIR", dir.path().join(".gently"))
            .assert()
            .success();
        let text = std::fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap();
        toml::from_str::<toml::Value>(&text).unwrap()
    };
    let initial = run(false);
    assert!(initial["mcp_servers"]["gently"].get("env").is_none());
    let opted_in = run(true);
    assert_eq!(
        opted_in["mcp_servers"]["gently"]["env"]["GENTLY_RESOLVE_RAW_VALUES"].as_str(),
        Some("1")
    );
    let reset = run(false);
    assert!(reset["mcp_servers"]["gently"].get("env").is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.path().join(".codex"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(dir.path().join(".codex/config.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

fn run_init(home: &std::path::Path, harness: &str) {
    Command::cargo_bin("gently")
        .unwrap()
        .args(["init", harness])
        .env("HOME", home)
        .env("GENTLY_STATE_DIR", home.join(".gently"))
        .env_remove("GENTLY_COLLECTOR_URL")
        .env_remove("GENTLY_TOKEN")
        .assert()
        .success();
}

#[test]
fn claude_refresh_registers_current_events_and_preserves_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::create_dir(home.join(".claude")).unwrap();
    let settings = serde_json::json!({
        "permissions": {"defaultMode": "plan"},
        "hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "user-policy", "timeout": 7}]}],
            "Notification": [{"hooks": [{"type": "command", "command": "user-notification"}]}]
        }
    });
    std::fs::write(home.join(".claude/settings.json"), settings.to_string()).unwrap();
    let mcp = serde_json::json!({
        "theme": "dark", "mcpServers": {
            "user-server": {"command": "user-mcp", "args": ["custom"]},
            "gently": {"type": "stdio", "command": "old-gently", "args": ["mcp"],
                "timeout": 13, "disabled": true, "env": {"USER_OPTION": "enabled"}}
        }
    });
    std::fs::write(home.join(".claude.json"), mcp.to_string()).unwrap();
    run_init(home, "--claude");
    let first: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap())
            .unwrap();
    for event in [
        "PreCompact",
        "PostCompact",
        "PostToolBatch",
        "PostModelSwitch",
        "PermissionRequest",
    ] {
        assert_eq!(
            first["hooks"][event].as_array().unwrap().len(),
            1,
            "{event}"
        );
    }
    assert_eq!(first["permissions"], settings["permissions"]);
    assert_eq!(
        first["hooks"]["PreToolUse"][0],
        settings["hooks"]["PreToolUse"][0]
    );
    assert_eq!(
        first["hooks"]["Notification"],
        settings["hooks"]["Notification"]
    );
    let installed: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
    assert_eq!(
        installed["mcpServers"]["user-server"],
        mcp["mcpServers"]["user-server"]
    );
    assert_eq!(installed["mcpServers"]["gently"]["timeout"], 13);
    assert_eq!(installed["mcpServers"]["gently"]["disabled"], true);
    assert_eq!(
        installed["mcpServers"]["gently"]["env"]["USER_OPTION"],
        "enabled"
    );
    assert_eq!(installed["theme"], "dark");
    run_init(home, "--claude");
    let second: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap())
            .unwrap();
    assert_eq!(first, second, "repeat init must not duplicate new events");
}

#[test]
fn codex_refresh_registers_current_events_and_preserves_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::create_dir(home.join(".codex")).unwrap();
    let original = r#"# retain my config comments
model = "fixture-model"
[features]
multi_agent = true
[projects."/workspace/demo"]
trust_level = "trusted"
[mcp_servers.user_server]
command = "user-mcp"
[mcp_servers.gently]
command = "old-gently"
args = ["mcp"]
enabled = false
startup_timeout_sec = 13
[mcp_servers.gently.env]
USER_OPTION = "enabled"
[[hooks.PreToolUse]]
matcher = "Bash"
[[hooks.PreToolUse.hooks]]
type = "command"
command = "user-policy"
timeout = 7
"#;
    std::fs::write(home.join(".codex/config.toml"), original).unwrap();
    run_init(home, "--codex");
    let first_text = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
    assert!(first_text.contains("# retain my config comments"));
    let first: toml::Value = toml::from_str(&first_text).unwrap();
    for event in [
        "SessionEnd",
        "Interrupt",
        "PreCompact",
        "PostCompact",
        "PermissionRequest",
    ] {
        assert_eq!(
            first["hooks"][event].as_array().unwrap().len(),
            1,
            "{event}"
        );
    }
    assert_eq!(first["model"].as_str(), Some("fixture-model"));
    assert_eq!(first["features"]["multi_agent"].as_bool(), Some(true));
    assert_eq!(
        first["projects"]["/workspace/demo"]["trust_level"].as_str(),
        Some("trusted")
    );
    assert_eq!(
        first["mcp_servers"]["user_server"]["command"].as_str(),
        Some("user-mcp")
    );
    assert_eq!(
        first["mcp_servers"]["gently"]["enabled"].as_bool(),
        Some(false)
    );
    assert_eq!(
        first["mcp_servers"]["gently"]["startup_timeout_sec"].as_integer(),
        Some(13)
    );
    assert_eq!(
        first["mcp_servers"]["gently"]["env"]["USER_OPTION"].as_str(),
        Some("enabled")
    );
    assert_eq!(
        first["hooks"]["PreToolUse"][0]["matcher"].as_str(),
        Some("Bash")
    );
    assert_eq!(
        first["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str(),
        Some("user-policy")
    );
    run_init(home, "--codex");
    let second_text = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
    assert_eq!(
        first_text, second_text,
        "repeat init must preserve comments and entries"
    );
}

#[test]
fn new_scaffold_uses_inherited_token_and_never_serializes_runtime_token() {
    let dir = tempfile::tempdir().unwrap();
    Command::cargo_bin("gently")
        .unwrap()
        .args(["init", "--claude"])
        .env("HOME", dir.path())
        .env("GENTLY_STATE_DIR", dir.path().join(".gently"))
        .env("GENTLY_TOKEN", "synthetic-runtime-token")
        .assert()
        .success();
    let text = std::fs::read_to_string(dir.path().join(".gently/config.toml")).unwrap();
    let config: toml::Value = toml::from_str(&text).unwrap();
    assert!(
        config.get("token").is_none(),
        "new config must not contain a token key"
    );
    assert!(text.contains("GENTLY_TOKEN"));
    assert!(!text.contains("CHANGE_ME"));
    assert!(!text.contains("synthetic-runtime-token"));
}

#[test]
fn init_never_rewrites_existing_gently_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".gently")).unwrap();
    let path = dir.path().join(".gently/config.toml");
    let original = "# user preferences\ncollector_url = 'http://127.0.0.1:8787'\ntenant_id = 'personal'\nprefer_quic = false\n";
    std::fs::write(&path, original).unwrap();
    run_init(dir.path(), "--claude");
    run_init(dir.path(), "--codex");
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn hook_command_quotes_shell_metacharacters_in_binary_path() {
    use std::io::Write;
    use std::process::{Command as ProcessCommand, Stdio};
    let dir = tempfile::tempdir().unwrap();
    let bin_dir = dir
        .path()
        .join("bin space's $GENTLY_QUOTE_TEST `printf unexpected`");
    std::fs::create_dir(&bin_dir).unwrap();
    let exe = bin_dir.join("gently");
    std::fs::copy(assert_cmd::cargo::cargo_bin("gently"), &exe).unwrap();
    for harness in ["--claude", "--codex"] {
        let home = tempfile::tempdir().unwrap();
        ProcessCommand::new(&exe)
            .args(["init", harness])
            .env("HOME", home.path())
            .env("GENTLY_STATE_DIR", home.path().join(".gently"))
            .env_remove("GENTLY_TOKEN")
            .env_remove("GENTLY_COLLECTOR_URL")
            .output()
            .unwrap()
            .status
            .success()
            .then_some(())
            .expect("fixture init succeeds");
        let command = if harness == "--claude" {
            let config: Value = serde_json::from_str(
                &std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap(),
            )
            .unwrap();
            config["hooks"]["SessionStart"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .to_owned()
        } else {
            let config: toml::Value = toml::from_str(
                &std::fs::read_to_string(home.path().join(".codex/config.toml")).unwrap(),
            )
            .unwrap();
            config["hooks"]["SessionStart"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let mut child = ProcessCommand::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .env("HOME", home.path())
            .env("GENTLY_STATE_DIR", home.path().join(".gently"))
            .env("GENTLY_QUOTE_TEST", "must-not-expand")
            .env_remove("GENTLY_TOKEN")
            .env_remove("GENTLY_COLLECTOR_URL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(br#"{"hook_event_name":"SessionStart","session_id":"quote-fixture","cwd":"/fixture"}"#).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "configured hook must execute the literal binary path"
        );
        assert!(
            output.stdout.is_empty(),
            "hook must stay advisory with no control output"
        );
        assert!(home
            .path()
            .join(".gently/tenants/personal/devices/local/state.db")
            .exists());
    }
}

#[test]
fn codex_inline_preferences_keep_user_env_and_clear_only_raw_opt_in() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".codex")).unwrap();
    let path = dir.path().join(".codex/config.toml");
    std::fs::write(&path, r#"features = { multi_agent = true }
[mcp_servers]
gently = { command = "old-gently", args = ["mcp"], enabled = false, env = { USER_OPTION = "enabled", GENTLY_RESOLVE_RAW_VALUES = "1" } }
"#).unwrap();
    run_init(dir.path(), "--codex");
    let parse = || toml::from_str::<toml::Value>(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let config = parse();
    assert_eq!(config["features"]["multi_agent"].as_bool(), Some(true));
    assert_eq!(config["features"]["hooks"].as_bool(), Some(true));
    assert_eq!(
        config["mcp_servers"]["gently"]["enabled"].as_bool(),
        Some(false)
    );
    assert_eq!(
        config["mcp_servers"]["gently"]["env"]["USER_OPTION"].as_str(),
        Some("enabled")
    );
    assert!(config["mcp_servers"]["gently"]["env"]
        .get("GENTLY_RESOLVE_RAW_VALUES")
        .is_none());
    Command::cargo_bin("gently")
        .unwrap()
        .args(["init", "--codex", "--resolve-raw-values"])
        .env("HOME", dir.path())
        .env("GENTLY_STATE_DIR", dir.path().join(".gently"))
        .env_remove("GENTLY_TOKEN")
        .env_remove("GENTLY_COLLECTOR_URL")
        .assert()
        .success();
    let config = parse();
    assert_eq!(
        config["mcp_servers"]["gently"]["env"]["USER_OPTION"].as_str(),
        Some("enabled")
    );
    assert_eq!(
        config["mcp_servers"]["gently"]["env"]["GENTLY_RESOLVE_RAW_VALUES"].as_str(),
        Some("1")
    );
    run_init(dir.path(), "--codex");
    assert!(parse()["mcp_servers"]["gently"]["env"]
        .get("GENTLY_RESOLVE_RAW_VALUES")
        .is_none());
}
