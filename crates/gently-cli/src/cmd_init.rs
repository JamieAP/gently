//! `gently init --claude/--codex` - install hooks and MCP into a harness.
//!
//! Idempotent: merges into the existing config files, preserving every other
//! key, and skips entries that already point at this binary.
//! Human-facing output goes to stderr (stdout stays clean).

use crate::config::Config;
use anyhow::{Context, Result};
use gently_store::private_fs::{ensure_private_dir, harden_existing_file, write_private_file};
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
    "PermissionRequest",
    "PreCompact",
    "PostCompact",
    "PostToolBatch",
    "PostModelSwitch",
];

pub fn run_claude(resolve_raw_values: bool) -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let exe = std::env::current_exe().context("cannot resolve current executable")?;

    scaffold_config(&cfg)?;
    let added_hooks = install_hooks(&home, &exe)?;
    install_mcp(&home, &exe, resolve_raw_values)?;

    eprintln!("gently: installed {added_hooks} hook event(s) into ~/.claude/settings.json");
    eprintln!("gently: registered MCP server 'gently' in ~/.claude.json");
    eprintln!(
        "gently: config at {}",
        cfg.state_dir.join("config.toml").display()
    );
    eprintln!("gently: set collector_url there or GENTLY_COLLECTOR_URL, and inherit GENTLY_TOKEN from your secret launcher, then restart your session");
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
    // is a collector_url with GENTLY_TOKEN inherited at runtime.
    let template = "# gently configuration\n\
        #\n\
        # Collector for export and queries; override with GENTLY_COLLECTOR_URL.\n\
        # Start the local collector before exporting or querying.\n\
        collector_url = \"http://127.0.0.1:8787\"\n\
        # Inherit GENTLY_TOKEN from your secret launcher. Keep tokens out of this file.\n\
        \n\
        # OPTIONAL tunables (shown with their defaults; uncomment to change):\n\
        #\n\
        # prefer_quic = true          # prefer HTTP/3 (QUIC) for export, fall back to HTTP/2\n\
        # outbox_cap = 10000          # max buffered envelope rows before the oldest are dropped\n\
        # export_batch = 512          # OTLP envelope rows per export request\n\
        # export_timeout_secs = 15    # per-request export timeout\n\
        # query_timeout_secs = 30     # per-request query / MCP timeout\n\
        # tenant_id = \"personal\"      # must match the collector principal\n\
        # device_id = \"local\"         # distinct capture host within the tenant\n\
        # capture_raw_values = false  # retain encrypted raw values using public reader policy\n\
        # sync_raw_values = false     # upload opaque ciphertext to the collector\n\
        # resolve_raw_values = false  # decrypt for CLI/MCP on an enrolled reader\n\
        # raw_manifest = \"/path/to/signed-manifest.json\"\n\
        # raw_trust = \"/path/to/trust.json\"\n\
        # raw_identity = \"/path/to/reader.age\"\n";
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

fn install_mcp(home: &Path, exe: &Path, resolve_raw_values: bool) -> Result<()> {
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
    let server = servers.entry("gently").or_insert_with(|| json!({}));
    let server = server
        .as_object_mut()
        .context("gently MCP server is not an object")?;
    server.insert("type".into(), json!("stdio"));
    server.insert("command".into(), json!(exe.to_string_lossy()));
    server.insert("args".into(), json!(["mcp"]));
    if resolve_raw_values {
        let env = server.entry("env").or_insert_with(|| json!({}));
        let env = env
            .as_object_mut()
            .context("gently MCP env is not an object")?;
        env.insert("GENTLY_RESOLVE_RAW_VALUES".into(), json!("1"));
    } else if let Some(env) = server.get_mut("env") {
        let env = env
            .as_object_mut()
            .context("gently MCP env is not an object")?;
        env.remove("GENTLY_RESOLVE_RAW_VALUES");
        if env.is_empty() {
            server.remove("env");
        }
    }
    write_json(&path, &config)
}

/// Keep managed hook groups and preferences intact. Exact commands match.
fn hooks_contains_command(arr: &mut [Value], command: &str) -> bool {
    let mut found = false;
    for entry in arr {
        if let Some(inner) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
            for handler in inner {
                if handler
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|existing| existing == command)
                {
                    handler["command"] = json!(command);
                    found = true;
                }
            }
        }
    }
    found
}

fn read_json(path: &Path) -> Result<Value> {
    harden_existing_file(path)?;
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))
        }
        Ok(_) => Ok(json!({})),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(_) => anyhow::bail!(
            "cannot read harness JSON configuration at {}",
            path.display()
        ),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        // The home directory is not a harness-owned state directory.
        if parent
            .file_name()
            .is_some_and(|name| name == ".claude" || name == ".codex")
        {
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

/// POSIX single quoting keeps whitespace, quotes, expansion and substitutions
/// literal. An embedded apostrophe closes the quoted word, adds one escaped
/// apostrophe, then opens it again.
fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', "'\"'\"'"))
}

/// Supported Codex 0.160.0 events. No tool matcher limits collection. The
/// PermissionRequest marker emits no verdict and leaves approval flow intact.
const CODEX_MODELED_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "Stop",
    "Interrupt",
    "PreToolUse",
    "PostToolUse",
    "SubagentStart",
    "SubagentStop",
    "PermissionRequest",
    "PreCompact",
    "PostCompact",
];

pub fn run_codex(resolve_raw_values: bool) -> Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    let home = dirs::home_dir().context("cannot determine home directory")?;
    let exe = std::env::current_exe().context("cannot resolve current executable")?;

    scaffold_config(&cfg)?;

    let path = home.join(".codex").join("config.toml");
    let mut doc = read_toml_doc(&path)?;
    let command = format!("{} hook --harness codex", shell_quote(&exe));
    let added = merge_codex_hooks(&mut doc, &command)?;
    ensure_codex_mcp(&mut doc, &exe.to_string_lossy(), resolve_raw_values)?;
    ensure_features_hooks(&mut doc)?;
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
    eprintln!("gently: set collector_url there or GENTLY_COLLECTOR_URL, and inherit GENTLY_TOKEN from your secret launcher, then restart Codex");
    Ok(())
}

/// Add a `[[hooks.<Event>]]`/`[[hooks.<Event>.hooks]]` command handler for each
/// modeled Codex event, skipping any event group that already runs our command.
/// Returns the number of events newly added.
fn merge_codex_hooks(doc: &mut DocumentMut, command: &str) -> Result<usize> {
    let mut added = 0;
    for event in CODEX_MODELED_EVENTS {
        if add_codex_hook_group(doc, event, command)? {
            added += 1;
        }
    }
    Ok(added)
}

/// TOML permits both inline and section tables. Normalize only the structures
/// that need appended hook arrays, and reject scalars before writing any file.
fn require_table<'a>(item: &'a mut Item, name: &str) -> Result<&'a mut Table> {
    if item.is_inline_table() {
        let decor = item.as_value().map(|value| value.decor().clone());
        *item = Item::Table(
            std::mem::take(item)
                .into_table()
                .map_err(|_| anyhow::anyhow!("{name} must be a table"))?,
        );
        if let Some(decor) = decor {
            *item.as_table_mut().unwrap().decor_mut() = decor;
        }
    }
    item.as_table_mut()
        .with_context(|| format!("{name} must be a table"))
}

fn require_root_table<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut Table> {
    let missing = !doc.contains_key(name);
    let inline = doc.get(name).is_some_and(Item::is_inline_table);
    let key_prefix = doc
        .get(name)
        .filter(|item| item.is_inline_table())
        .and_then(|_| doc.as_table().key(name))
        .and_then(|key| key.leaf_decor().prefix())
        .cloned();
    if inline {
        if let Some(mut key) = doc.as_table_mut().key_mut(name) {
            // The old assignment-key prefix belongs before the new section,
            // not inside `[section]` when the same key is rendered again.
            key.leaf_decor_mut().clear();
        }
    }
    let table = require_table(
        doc.entry(name).or_insert_with(|| Item::Table(Table::new())),
        name,
    )?;
    if missing {
        table.set_implicit(true);
    }
    if let Some(prefix) = key_prefix {
        table.decor_mut().set_prefix(prefix);
    }
    Ok(table)
}

fn require_groups<'a>(item: &'a mut Item, name: &str) -> Result<&'a mut ArrayOfTables> {
    if item.as_array().is_some_and(|array| array.is_empty()) {
        *item = Item::ArrayOfTables(ArrayOfTables::new());
    } else if item.is_array() {
        let previous = std::mem::take(item);
        match previous.into_array_of_tables() {
            Ok(groups) => *item = Item::ArrayOfTables(groups),
            Err(previous) => {
                *item = previous;
                anyhow::bail!("{name} must be an array of tables");
            }
        }
    }
    item.as_array_of_tables_mut()
        .with_context(|| format!("{name} must be an array of tables"))
}

/// Append one command-handler group under `hooks.<event>`. No `matcher` is set,
/// so tool events match every tool. Idempotent: returns false if a group already
/// points at `command`.
fn add_codex_hook_group(doc: &mut DocumentMut, event: &str, command: &str) -> Result<bool> {
    let hooks = require_root_table(doc, "hooks")?;
    let groups = require_groups(
        hooks
            .entry(event)
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new())),
        &format!("hooks.{event}"),
    )?;

    let mut found = false;
    for group in groups.iter_mut() {
        found |= group_has_command(group, command, event)?;
    }
    if found {
        return Ok(false);
    }

    let mut handler = Table::new();
    handler["type"] = value("command");
    handler["command"] = value(command);
    let mut inner = ArrayOfTables::new();
    inner.push(handler);

    let mut group = Table::new();
    group.insert("hooks", Item::ArrayOfTables(inner));
    groups.push(group);
    Ok(true)
}

/// Match managed handlers while preserving their matcher and options.
fn group_has_command(group: &mut Table, command: &str, event: &str) -> Result<bool> {
    let Some(inner) = group.get_mut("hooks") else {
        return Ok(false);
    };
    let inner = require_groups(inner, &format!("hooks.{event}.hooks"))?;
    let mut found = false;
    for handler in inner.iter_mut() {
        if handler
            .get("command")
            .and_then(Item::as_str)
            .is_some_and(|existing| existing == command)
        {
            handler["command"] = value(command);
            found = true;
        }
    }
    Ok(found)
}

/// Update owned executable/args/raw-resolution fields in `[mcp_servers.gently]`,
/// keeping the user's other MCP preferences and environment entries.
fn ensure_codex_mcp(doc: &mut DocumentMut, exe: &str, resolve_raw_values: bool) -> Result<()> {
    let servers = require_root_table(doc, "mcp_servers")?;
    let server = servers
        .entry("gently")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .context("mcp_servers.gently must be a table")?;
    server.insert("command", value(exe));
    let mut args = Array::new();
    args.push("mcp");
    server.insert("args", value(args));
    if resolve_raw_values {
        let env = server
            .entry("env")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_like_mut()
            .context("mcp_servers.gently.env must be a table")?;
        env.insert("GENTLY_RESOLVE_RAW_VALUES", value("1"));
    } else if let Some(env) = server.get_mut("env") {
        let env = env
            .as_table_like_mut()
            .context("mcp_servers.gently.env must be a table")?;
        env.remove("GENTLY_RESOLVE_RAW_VALUES");
        if env.is_empty() {
            server.remove("env");
        }
    }
    Ok(())
}

/// Ensure `[features] hooks = true` without disturbing other feature flags.
fn ensure_features_hooks(doc: &mut DocumentMut) -> Result<()> {
    let features = doc
        .entry("features")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .context("features must be a table")?;
    features.insert("hooks", value(true));
    Ok(())
}

/// Read a TOML document for in-place editing, or start a fresh one if absent.
fn read_toml_doc(path: &Path) -> Result<DocumentMut> {
    harden_existing_file(path)?;
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => s.parse::<DocumentMut>().map_err(|_| {
            anyhow::anyhow!(
                "invalid Codex configuration TOML at {}; inspect the file before retrying init",
                path.display()
            )
        }),
        Ok(_) => Ok(DocumentMut::new()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(_) => anyhow::bail!("cannot read Codex configuration at {}", path.display()),
    }
}

/// Atomically write a TOML document (temp sibling + rename), preserving format.
fn write_toml_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    if let Some(parent) = path.parent() {
        // The home directory is not a harness-owned state directory.
        if parent
            .file_name()
            .is_some_and(|name| name == ".claude" || name == ".codex")
        {
            ensure_private_dir(parent)?;
        }
    }
    let tmp: PathBuf = path.with_extension("gently-tmp");
    write_private_file(&tmp, doc.to_string())
        .with_context(|| format!("writing {}", tmp.display()))?;
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
        let added =
            merge_codex_hooks(&mut doc, "/usr/local/bin/gently hook --harness codex").unwrap();
        ensure_codex_mcp(&mut doc, "/usr/local/bin/gently", false).unwrap();
        ensure_features_hooks(&mut doc).unwrap();
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
        assert!(!out.contains("GENTLY_RESOLVE_RAW_VALUES"));
        assert_eq!(added, CODEX_MODELED_EVENTS.len());

        let _: toml::Value = toml::from_str(&out).unwrap();
    }

    #[test]
    fn merge_is_idempotent() {
        let mut doc: toml_edit::DocumentMut = EXISTING.parse().unwrap();
        let cmd = "/usr/local/bin/gently hook --harness codex";
        let first = merge_codex_hooks(&mut doc, cmd).unwrap();
        let second = merge_codex_hooks(&mut doc, cmd).unwrap();
        assert_eq!(first, CODEX_MODELED_EVENTS.len());
        assert_eq!(second, 0, "second merge adds nothing");
        let groups = doc["hooks"]["PreToolUse"].as_array_of_tables().unwrap();
        assert_eq!(groups.len(), 1);
    }
}
