//! `gently init --claude` - install the hooks and MCP server into a harness.
//!
//! Idempotent: merges into the existing config files, preserving every other
//! key, and skips entries that already point at this binary. Structured so a
//! future `--codex` / `--cursor` flag adds its own adapter without disturbing
//! this one. All human-facing output goes to stderr (stdout stays clean).

use crate::config::Config;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Hook events gently models. Unmodeled events are still captured as markers if
/// the user wires them, but these are the ones init installs by default.
const MODELED_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "SubagentStart",
    "SubagentStop",
];

pub fn run_claude() -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let exe = std::env::current_exe().context("cannot resolve current executable")?;

    scaffold_config(&cfg)?;
    let added_hooks = install_hooks(&home, &exe)?;
    install_mcp(&home, &exe)?;

    eprintln!("gently: installed {added_hooks} hook event(s) into ~/.claude/settings.json");
    eprintln!("gently: registered MCP server 'gently' in ~/.claude.json");
    eprintln!(
        "gently: config at {}",
        cfg.state_dir.join("config.toml").display()
    );
    eprintln!("gently: set collector_url + token there (or via GENTLY_COLLECTOR_URL / GENTLY_TOKEN), then restart your session");
    Ok(())
}

/// Write a config template if none exists. Never overwrites an existing config.
fn scaffold_config(cfg: &Config) -> Result<()> {
    let path = cfg.state_dir.join("config.toml");
    if path.exists() {
        return Ok(());
    }
    let template = "# gently collector configuration\n\
        # The Cloudflare Worker base URL and shared bearer token.\n\
        collector_url = \"http://127.0.0.1:8787\"\n\
        token = \"CHANGE_ME\"\n";
    std::fs::write(&path, template).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn install_hooks(home: &Path, exe: &Path) -> Result<usize> {
    let path = home.join(".claude").join("settings.json");
    let mut settings = read_json(&path)?;
    let command = format!("{} hook", shell_quote(exe));

    let hooks = settings
        .as_object_mut()
        .context("settings.json is not an object")?
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().context("hooks is not an object")?;

    let mut added = 0;
    for event in MODELED_EVENTS {
        let arr = hooks.entry(*event).or_insert_with(|| json!([]));
        let arr = arr.as_array_mut().context("hook event is not an array")?;
        if hooks_contains_command(arr, &command) {
            continue;
        }
        arr.push(json!({"hooks": [{"type": "command", "command": command}]}));
        added += 1;
    }

    write_json(&path, &settings)?;
    Ok(added)
}

fn install_mcp(home: &Path, exe: &Path) -> Result<()> {
    let path = home.join(".claude.json");
    let mut config = read_json(&path)?;
    let servers = config
        .as_object_mut()
        .context(".claude.json is not an object")?
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    let servers = servers
        .as_object_mut()
        .context("mcpServers is not an object")?;
    servers.insert(
        "gently".to_string(),
        json!({"type": "stdio", "command": exe.to_string_lossy(), "args": ["mcp"]}),
    );
    write_json(&path, &config)
}

/// True if any entry in this event's hook list already runs our command.
fn hooks_contains_command(arr: &[Value], command: &str) -> bool {
    arr.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(Value::as_array)
            .map(|inner| {
                inner
                    .iter()
                    .any(|h| h.get("command").and_then(Value::as_str) == Some(command))
            })
            .unwrap_or(false)
    })
}

fn read_json(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))
        }
        _ => Ok(json!({})),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let body = serde_json::to_string_pretty(value)?;
    // Write to a temp sibling then rename for an atomic update.
    let tmp: PathBuf = path.with_extension("gently-tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

fn shell_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.into_owned()
    }
}
