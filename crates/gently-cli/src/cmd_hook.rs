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
use gently_core::{OtlpRequest, Resource, Span, SpanId, SpanKind, Status, TraceId};
use gently_harness::{apply, ClaudeCode, Codex, Harness, Parsed};
use gently_store::Store;
use std::io::Read;
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
        if process(harness).is_err() {
            // Parsing/encryption errors may incorporate attacker-controlled
            // input. Only a fixed diagnostic can reach a persistent log.
            tracing::error!("hook processing failed; check configuration and recipient policy");
        }
    });
}

fn process(harness: HarnessKind) -> anyhow::Result<()> {
    let cfg = Config::load()?;
    cfg.ensure_state_dir()?;
    crate::logging::init_file_log(&cfg.runtime_dir().join("hook.log"));

    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw)?;
    let value = crate::json_fidelity::parse(&raw)?;

    let store = Store::open(&cfg.state_db())?;

    let adapter: &dyn Harness = match harness {
        HarnessKind::Claude => &ClaudeCode,
        HarnessKind::Codex => &Codex,
    };
    let mut parsed = adapter.parse(&value)?;
    let mut metadata_parsed = parsed.clone();
    local_raw::metadata_only(&mut metadata_parsed);
    let prepared_raw = match local_raw::prepare(&cfg, &value, &mut parsed, adapter.name()) {
        Ok(prepared) => prepared,
        Err(_) => {
            tracing::warn!("encrypted raw capture unavailable; preserving length-only telemetry");
            None
        }
    };
    // `tmux_pane` is filled from `$TMUX_PANE` inside `Resource::new`; the
    // transcript path rides in from the payload so a pane→session query also
    // yields the exact session file.
    let resource = Resource::new(&parsed.session_id, adapter.name(), &parsed.cwd)
        .with_transcript_path(parsed.transcript_path.as_deref().unwrap_or_default());

    let capturing = prepared_raw.is_some();
    let result = store.transaction::<_, anyhow::Error>(|store| {
        enqueue_event(store, &cfg, &parsed, &value, &resource, prepared_raw)
    });
    if capturing && result.is_err() {
        // The ciphertext transaction has rolled back every lifecycle write and
        // ref. An oversized payload or expired policy must not lose telemetry.
        tracing::warn!("encrypted raw capture unavailable; preserving length-only telemetry");
        store.transaction::<_, anyhow::Error>(|store| {
            enqueue_event(store, &cfg, &metadata_parsed, &value, &resource, None)
        })?;
    } else {
        result?;
    }

    // Reap provisional spans after crashes, older harness releases or missing
    // lifecycle hooks so the local table cannot grow without bound. Cheap single
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

fn enqueue_event(
    store: &Store,
    cfg: &Config,
    parsed: &Parsed,
    value: &serde_json::Value,
    resource: &Resource,
    prepared_raw: Option<local_raw::PreparedRaw>,
) -> anyhow::Result<()> {
    let now = now_nanos();
    let mut spans = apply(store, parsed, now)?;
    let mut receipt = hook_receipt(parsed, value, now)?;
    if let Some(raw) = &prepared_raw {
        receipt
            .attributes
            .push(("gently.hook_payload.raw_ref".into(), raw.reference().into()));
    }
    spans.push(receipt);
    if let Some(prepared_raw) = prepared_raw {
        store.raw_object_put(&prepared_raw.seal_for_spans(store, &spans)?)?;
    }
    // One durable envelope per hook commits with ciphertext and lifecycle state.
    if !spans.is_empty() {
        let mut req = OtlpRequest::single(resource, spans);
        for (key, value) in [
            ("gently.tenant_id", &cfg.tenant_id),
            ("gently.device_id", &cfg.device_id),
        ] {
            req.resource_spans[0]
                .resource
                .attributes
                .push(gently_core::otlp::KeyValue {
                    key: key.into(),
                    value: gently_core::otlp::AnyValue {
                        string_value: Some(value.clone()),
                        int_value: None,
                    },
                });
        }
        store.outbox_enqueue(&serde_json::to_string(&req)?)?;
    }
    Ok(())
}

fn hook_receipt(parsed: &Parsed, value: &serde_json::Value, now: u64) -> anyhow::Result<Span> {
    let bytes = serde_json::to_vec(value)?;
    let event = value["hook_event_name"].as_str().unwrap_or_default();
    let mut attrs = vec![
        ("gently.event".into(), event.into()),
        ("gently.hook_payload.bytes".into(), bytes.len().to_string()),
    ];
    for key in [
        "tool_name",
        "tool_use_id",
        "turn_id",
        "prompt_id",
        "agent_id",
    ] {
        if let Some(v) = value.get(key).and_then(serde_json::Value::as_str) {
            attrs.push((format!("gently.hook.{key}"), v.into()));
        }
    }
    // Retain queryable event metadata even when a later lifecycle update
    // replaces the aggregate's attrs (for example, resume cache estimates).
    // Correlation keys stay namespaced so receipts never enter tool rollups.
    for op in &parsed.ops {
        for (key, value) in op.observation_attrs() {
            let key = match key.as_str() {
                "gently.tool_name" | "gently.tool_use_id" | "gently.turn_id"
                | "gently.prompt_id" | "gently.agent_id" => {
                    key.replacen("gently.", "gently.hook.", 1)
                }
                _ => key.clone(),
            };
            if !attrs.iter().any(|(existing, _)| existing == &key) {
                attrs.push((key, value.clone()));
            }
        }
    }
    let parent_key = parsed
        .agent_id
        .as_ref()
        .map(|id| format!("agent:{id}"))
        .unwrap_or_else(|| "session".into());
    Ok(Span {
        trace_id: TraceId::from_session(&parsed.session_id),
        span_id: SpanId::derive(
            &parsed.session_id,
            &format!("hook:{now}:{}", std::process::id()),
        ),
        parent_span_id: Some(SpanId::derive(&parsed.session_id, &parent_key)),
        name: format!("hook:{event}"),
        kind: SpanKind::Internal,
        start_unix_nano: now,
        end_unix_nano: now,
        status: Status::Unset,
        attributes: attrs,
    })
}

/// Terminal events flush the exporter unconditionally so the last spans always
/// ship. Codex interrupts close a turn without claiming success or failure.
fn is_terminal_event(harness: HarnessKind, event: &str) -> bool {
    match harness {
        HarnessKind::Claude => matches!(event, "Stop" | "StopFailure" | "SessionEnd"),
        HarnessKind::Codex => matches!(event, "Stop" | "Interrupt" | "SessionEnd"),
    }
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
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
    // Desktop hooks often have no token. They queue for the persistent watcher
    // without repeated failed exporter processes or biometric prompts.
    if cfg.token.is_empty() || cfg.collector_url.is_empty() {
        return;
    }
    if !force && exporter_running(cfg) {
        return;
    }
    spawn_detached_export();
}

/// Non-blocking probe: is an exporter currently holding the lock? Acquiring then
/// immediately releasing is a cheap local file op; a held lock means "running".
fn exporter_running(cfg: &Config) -> bool {
    use fs4::fs_std::FileExt;
    let Ok(file) =
        gently_store::private_fs::open_private_file(&cfg.runtime_dir().join("export.lock"), false)
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
    // This build preserves history by default, but `exe` names a path that a
    // reinstall can repoint at an older version before the child starts, and
    // those versions trim queued history unless this flag is passed.
    cmd.arg("export")
        .arg("--preserve-backlog")
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
