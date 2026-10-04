//! `gently init --claude` - install the hooks and MCP server into a harness.
//!
//! Idempotent: merges into the existing config files, preserving every other
//! key, and skips entries that already point at this binary. Structured so a
//! future `--codex` / `--cursor` flag adds its own adapter without disturbing
//! this one. All human-facing output goes to stderr (stdout stays clean).

use crate::config::Config;
use gently_store::private_fs::{ensure_private_dir, harden_existing_file, write_private_file};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table};

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

pub fn run_claude(resolve_local_raw_values: bool) -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let exe = std::env::current_exe().context("cannot resolve current executable")?;

    scaffold_config(&cfg)?;
    let added_hooks = install_hooks(&home, &exe)?;
    install_mcp(&home, &exe, resolve_local_raw_values)?;

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
    // Fully-documented template. Required keys at top; tunables (commented) show
    // their built-in defaults so the file is self-explanatory. A minimal config
    // is just collector_url + token.
    let template = "# gently configuration  -  https://github.com (gently)\n\
        #\n\
        # REQUIRED: the collector (a Cloudflare Worker backed by D1, or a local\n\
        # `wrangler dev`). Both export and queries use these. Override at runtime\n\
        # with GENTLY_COLLECTOR_URL / GENTLY_TOKEN.\n\
        collector_url = \"https://gently-collector.<account>.workers.dev\"\n\
        token = \"CHANGE_ME\"\n\
        \n\
        # OPTIONAL tunables (shown with their defaults; uncomment to change):\n\
        #\n\
        # prefer_quic = true          # prefer HTTP/3 (QUIC) for export, fall back to HTTP/2\n\
        # outbox_cap = 10000          # max buffered spans before the oldest are dropped\n\
        # export_batch = 512          # spans coalesced into one export request\n\
        # export_timeout_secs = 15    # per-request export timeout\n\
        # query_timeout_secs = 30     # per-request query / MCP timeout\n";
    write_private_file(&path, template).with_context(|| format!("writing {}", path.display()))?;
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

fn install_mcp(home: &Path, exe: &Path, resolve_local_raw_values: bool) -> Result<()> {
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
    let mut server = json!({
        "type": "stdio", "command": exe.to_string_lossy(), "args": ["mcp"]
    });
    if resolve_local_raw_values {
        server["env"] = json!({"GENTLY_RESOLVE_LOCAL_SHA_RAW_VALUES": "1"});
    }
    servers.insert("gently".to_string(), server);
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
    harden_existing_file(path)?;
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))
        }
        _ => Ok(json!({})),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        // The home directory is not a harness-owned state directory.
        if parent.file_name().is_some_and(|name| name == ".claude" || name == ".codex") {
            ensure_private_dir(parent)?;
        }
    }
    let body = serde_json::to_string_pretty(value)?;
    // Write to a temp sibling then rename for an atomic update.
    let tmp: PathBuf = path.with_extension("gently-tmp");
    write_private_file(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
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

/// Codex hook events gently models. The tool events install with no `matcher`
/// so every tool is captured (not just one). Codex has no SessionEnd, so the
/// session root is finalized only provisionally (see design spec).
const CODEX_MODELED_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "PreToolUse",
    "PostToolUse",
    "SubagentStart",
    "SubagentStop",
];

pub fn run_codex(resolve_local_raw_values: bool) -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let exe = std::env::current_exe().context("cannot resolve current executable")?;

    scaffold_config(&cfg)?;

    let path = home.join(".codex").join("config.toml");
    let mut doc = read_toml_doc(&path)?;
    let command = format!("{} hook --harness codex", shell_quote(&exe));
    let added = merge_codex_hooks(&mut doc, &command);
    ensure_codex_mcp(&mut doc, &exe.to_string_lossy(), resolve_local_raw_values);
    ensure_features_hooks(&mut doc);
    write_toml_doc(&path, &doc)?;

    eprintln!(
        "gently: installed {added} Codex hook event(s) into {}",
        path.display()
    );
    eprintln!(
        "gently: registered MCP server 'gently' in {}",
        path.display()
    );
    eprintln!("gently: ACTION REQUIRED - run `/hooks` inside Codex and TRUST the gently entries;");
    eprintln!("gently:   non-managed command hooks do not fire until trusted, and ensure [features] hooks = true.");
    eprintln!(
        "gently: config at {}",
        cfg.state_dir.join("config.toml").display()
    );
    eprintln!("gently: set collector_url + token there (or via GENTLY_COLLECTOR_URL / GENTLY_TOKEN), then restart Codex");
    Ok(())
}

/// Add a `[[hooks.<Event>]]`/`[[hooks.<Event>.hooks]]` command handler for each
/// modeled Codex event, skipping any event group that already runs our command.
/// Returns the number of events newly added.
fn merge_codex_hooks(doc: &mut DocumentMut, command: &str) -> usize {
    let mut added = 0;
    for event in CODEX_MODELED_EVENTS {
        if add_codex_hook_group(doc, event, command) {
            added += 1;
        }
    }
    added
}

/// Append one command-handler group under `hooks.<event>`. No `matcher` is set,
/// so tool events match every tool. Idempotent: returns false if a group already
/// points at `command`.
fn add_codex_hook_group(doc: &mut DocumentMut, event: &str, command: &str) -> bool {
    let hooks = doc
        .entry("hooks")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .expect("hooks is a table");
    hooks.set_implicit(true);
    let groups = hooks
        .entry(event)
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .expect("event groups is an array-of-tables");

    if groups.iter().any(|g| group_has_command(g, command)) {
        return false;
    }

    let mut handler = Table::new();
    handler["type"] = value("command");
    handler["command"] = value(command);
    let mut inner = ArrayOfTables::new();
    inner.push(handler);

    let mut group = Table::new();
    group.insert("hooks", Item::ArrayOfTables(inner));
    groups.push(group);
    true
}

/// True if any handler in this group already runs `command`.
fn group_has_command(group: &Table, command: &str) -> bool {
    group
        .get("hooks")
        .and_then(Item::as_array_of_tables)
        .map(|inner| {
            inner
                .iter()
                .any(|h| h.get("command").and_then(Item::as_str) == Some(command))
        })
        .unwrap_or(false)
}

/// Register `[mcp_servers.gently]` with the gently binary, replacing any prior
/// entry of the same name (the command may have moved).
fn ensure_codex_mcp(doc: &mut DocumentMut, exe: &str, resolve_local_raw_values: bool) {
    let servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .expect("mcp_servers is a table");
    servers.set_implicit(true);
    let mut server = Table::new();
    server["command"] = value(exe);
    let mut args = Array::new();
    args.push("mcp");
    server["args"] = value(args);
    if resolve_local_raw_values {
        let mut env = Table::new();
        env["GENTLY_RESOLVE_LOCAL_SHA_RAW_VALUES"] = value("1");
        server.insert("env", Item::Table(env));
    }
    servers.insert("gently", Item::Table(server));
}

/// Ensure `[features] hooks = true` without disturbing other feature flags.
fn ensure_features_hooks(doc: &mut DocumentMut) {
    let features = doc
        .entry("features")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .expect("features is a table");
    features["hooks"] = value(true);
}

/// Read a TOML document for in-place editing, or start a fresh one if absent.
fn read_toml_doc(path: &Path) -> Result<DocumentMut> {
    harden_existing_file(path)?;
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => s
            .parse::<DocumentMut>()
            .with_context(|| format!("parsing {}", path.display())),
        _ => Ok(DocumentMut::new()),
    }
}

/// Atomically write a TOML document (temp sibling + rename), preserving format.
fn write_toml_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    if let Some(parent) = path.parent() {
        // The home directory is not a harness-owned state directory.
        if parent.file_name().is_some_and(|name| name == ".claude" || name == ".codex") {
            ensure_private_dir(parent)?;
        }
    }
    let tmp: PathBuf = path.with_extension("gently-tmp");
    write_private_file(&tmp, doc.to_string()).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod codex_tests {
    use super::*;

    const EXISTING: &str = r#"# my codex config
model = "gpt-5.5"

[features]
multi_agent = true

[projects."/workspace/demo"]
trust_level = "trusted"
"#;

    #[test]
    fn merge_preserves_existing_keys_comments_and_adds_hooks_and_mcp() {
        let mut doc: toml_edit::DocumentMut = EXISTING.parse().unwrap();
        let added = merge_codex_hooks(&mut doc, "/usr/local/bin/gently hook --harness codex");
        ensure_codex_mcp(&mut doc, "/usr/local/bin/gently", false);
        ensure_features_hooks(&mut doc);
        let out = doc.to_string();

        assert!(out.contains("# my codex config"), "comment preserved");
        assert!(out.contains("model = \"gpt-5.5\""));
        assert!(out.contains("multi_agent = true"));
        assert!(out.contains("[projects.\"/workspace/demo\"]"));
        assert!(out.contains("hooks = true"));
        assert!(out.contains("[[hooks.PreToolUse]]"));
        assert!(out.contains("[[hooks.PreToolUse.hooks]]"));
        assert!(out.contains("type = \"command\""));
        assert!(out.contains("gently hook --harness codex"));
        assert!(out.contains("[mcp_servers.gently]"));
        assert!(out.contains("args = [\"mcp\"]"));
        assert!(!out.contains("GENTLY_RESOLVE_LOCAL_SHA_RAW_VALUES"));
        assert_eq!(added, CODEX_MODELED_EVENTS.len());

        let _: toml::Value = toml::from_str(&out).unwrap();
    }

    #[test]
    fn merge_is_idempotent() {
        let mut doc: toml_edit::DocumentMut = EXISTING.parse().unwrap();
        let cmd = "/usr/local/bin/gently hook --harness codex";
        let first = merge_codex_hooks(&mut doc, cmd);
        let second = merge_codex_hooks(&mut doc, cmd);
        assert_eq!(first, CODEX_MODELED_EVENTS.len());
        assert_eq!(second, 0, "second merge adds nothing");
        let groups = doc["hooks"]["PreToolUse"].as_array_of_tables().unwrap();
        assert_eq!(groups.len(), 1);
    }
}
