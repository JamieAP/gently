//! `gently hook` - the harness hook entrypoint.
//!
//! Hard contract: this command writes **nothing** to stdout (Claude Code parses
//! hook stdout as JSON control output; a stray byte corrupts the live session)
//! and **always exits 0** (a non-zero exit can break the user's session). All
//! diagnostics go to `<state_dir>/hook.log`. This is the one deliberate
//! exception to the repo's let-it-crash rule: internally we still fail fast and
//! log, but never surface a fault that could disrupt the harness.

use crate::config::Config;
use crate::local_raw;
use crate::HarnessKind;
use gently_core::{OtlpRequest, Resource};
use gently_harness::{apply, ClaudeCode, Codex, Harness};
use gently_store::Store;
use std::io::{Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

/// How long a provisional open span lingers before the reaper drops its local
/// bookkeeping. Sized well past any real session (a day) so a live but quiet
/// session is never reaped out from under itself; the provisional span already
/// lives in the collector regardless.
const OPEN_SPAN_TTL_NANOS: u64 = 24 * 3600 * 1_000_000_000;

/// Run the hook. Never returns an error to the caller; never touches stdout.
pub fn run(harness: HarnessKind) {
    // Catch every panic so a bug in our span logic cannot ever break the
    // harness session. The result is logged and discarded; exit stays 0.
    let _ = std::panic::catch_unwind(|| {
        if let Err(e) = process(harness) {
            tracing::error!(error = %e, "hook processing failed");
        }
    });
}

fn process(harness: HarnessKind) -> anyhow::Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    crate::logging::init_file_log(&cfg.state_dir.join("hook.log"));

    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let value: serde_json::Value = serde_json::from_str(&raw)?;

    if std::env::var_os("GENTLY_DEBUG").is_some() {
        capture_raw(&cfg, harness, &value, &raw);
    }

    let store = Store::open(&cfg.state_db())?;
    if let Err(e) = local_raw::capture_hook_values(&store, &value) {
        tracing::warn!(error = %e, "local raw value capture failed");
    }

    let adapter: &dyn Harness = match harness {
        HarnessKind::Claude => &ClaudeCode,
        HarnessKind::Codex => &Codex,
    };
    let parsed = adapter.parse(&value)?;
    // `tmux_pane` is filled from `$TMUX_PANE` inside `Resource::new`; the
    // transcript path rides in from the payload so a pane→session query also
    // yields the exact session file.
    let resource = Resource::new(&parsed.session_id, adapter.name(), &parsed.cwd)
        .with_transcript_path(parsed.transcript_path.as_deref().unwrap_or_default());

    let spans = apply(&store, &parsed, now_nanos())?;

    for span in spans {
        let req = OtlpRequest::single(&resource, vec![span]);
        store.outbox_enqueue(&serde_json::to_string(&req)?)?;
    }

    // Reap provisional open spans whose close never came (Codex has no
    // SessionEnd, and fires no Stop for an Esc-interrupted turn - openai/codex
    // #22858), so the local table cannot grow without bound. Cheap single
    // DELETE; best-effort so it never disrupts the hook.
    let cutoff = now_nanos().saturating_sub(OPEN_SPAN_TTL_NANOS);
    match store.reap_open_spans(cutoff) {
        Ok(n) if n > 0 => tracing::info!(reaped = n, "reaped stale open spans"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "open-span reap failed"),
    }

    // Terminal events flush unconditionally so the last spans always ship;
    // other events throttle (skip the spawn if an exporter is already running).
    let event = value
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    maybe_spawn_export(&cfg, is_terminal_event(harness, event));
    Ok(())
}

/// Terminal events flush the exporter unconditionally so the last spans always
/// ship. The set is harness-specific: Codex has no `SessionEnd`/`StopFailure`.
fn is_terminal_event(harness: HarnessKind, event: &str) -> bool {
    match harness {
        HarnessKind::Claude => matches!(event, "Stop" | "StopFailure" | "SessionEnd"),
        HarnessKind::Codex => event == "Stop",
    }
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Append the raw event payload to `<state_dir>/raw/<harness>/<event>.jsonl` and
/// refresh a redacted env snapshot at `<state_dir>/raw/<harness>/env.json`.
/// Namespacing by harness keeps Claude and Codex payloads separable for coverage
/// audits (the payload itself has no harness field); the env snapshot records
/// exactly what each harness hands the hook process. Best-effort: this
/// schema-verification debug aid (gated on `GENTLY_DEBUG`) must never interfere
/// with the pipeline.
fn capture_raw(cfg: &Config, harness: HarnessKind, value: &serde_json::Value, raw: &str) {
    let hname = match harness {
        HarnessKind::Claude => "claude",
        HarnessKind::Codex => "codex",
    };
    let event = value
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let dir = cfg.state_dir.join("raw").join(hname);
    if gently_store::private_fs::ensure_private_dir(&cfg.state_dir.join("raw")).is_err()
        || gently_store::private_fs::ensure_private_dir(&dir).is_err() {
        return;
    }
    // Hook event names are input; prevent them from escaping the raw directory.
    if !event.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') { return; }
    if let Ok(mut f) = gently_store::private_fs::open_private_file(&dir.join(format!("{event}.jsonl")), true)
    {
        let _ = writeln!(f, "{}", raw.trim());
    }
    if let Ok(env_json) = serde_json::to_string_pretty(&redacted_env()) {
        let _ = gently_store::private_fs::write_private_file(&dir.join("env.json"), env_json);
    }
}

/// A process-environment snapshot with filtering by variable name.
/// Only names containing the listed markers are masked; secrets with other
/// names remain in the snapshot. This denylist is not a complete secret detector.
fn redacted_env() -> std::collections::BTreeMap<String, String> {
    const SECRET_MARKERS: [&str; 6] = ["KEY", "TOKEN", "SECRET", "PASSWORD", "AUTH", "CREDENTIAL"];
    std::env::vars()
        .map(|(k, v)| {
            let upper = k.to_ascii_uppercase();
            if SECRET_MARKERS.iter().any(|m| upper.contains(m)) {
                (k, "<redacted>".to_string())
            } else {
                (k, v)
            }
        })
        .collect()
}

/// Spawn the detached exporter, unless throttled.
///
/// Hot-path guard: the exporter is a separate process, so spawning one on every
/// hook would fork+exec on every tool call (and under parallel tools, many at
/// once). When `force` is false we first probe the exporter lock - if an
/// exporter already holds it, we skip the spawn entirely, because that running
/// exporter drains in a loop until the outbox is empty and will pick up the row
/// we just enqueued. `force` (terminal events) always spawns so the final flush
/// is guaranteed even if the previous exporter had already moved past our row.
fn maybe_spawn_export(cfg: &Config, force: bool) {
    if !force && exporter_running(cfg) {
        return;
    }
    spawn_detached_export();
}

/// Non-blocking probe: is an exporter currently holding the lock? Acquiring then
/// immediately releasing is a cheap local file op; a held lock means "running".
fn exporter_running(cfg: &Config) -> bool {
    use fs4::fs_std::FileExt;
    let Ok(file) = gently_store::private_fs::open_private_file(&cfg.state_dir.join("export.lock"), false)
    else {
        return false; // can't tell → don't suppress the spawn
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            false
        }
        Err(_) => true, // held by a running exporter
    }
}

/// Launch `gently export` fully detached so the hook never blocks on the
/// network. A new process group keeps it clear of the shell's job-control
/// signals; the child reparents to init when this process exits.
fn spawn_detached_export() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("export")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    // Spawn and forget: do not wait. A failed spawn is non-fatal - the next
    // hook will try again and the outbox is durable.
    let _ = cmd.spawn();
}
