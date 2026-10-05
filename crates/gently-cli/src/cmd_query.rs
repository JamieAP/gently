//! `gently traces|trace|spans|stats` - query the collector and render results.

use crate::config::Config;
use crate::query_client::{QueryClient, SpanFilters, SpanRow, TraceFilters};
use anyhow::{Context, Result};
use comfy_table::{Cell, Table};
use std::collections::HashMap;

/// Output format for query commands.
#[derive(Clone, Copy)]
pub enum Format {
    Table,
    Json,
}

#[derive(Clone, Copy)]
pub enum TraceFormat {
    Tree,
    Json,
    Waterfall,
}

impl Format {
    pub fn from_json_flag(json: bool) -> Self {
        if json {
            Format::Json
        } else {
            Format::Table
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

pub fn traces(filters: TraceFilters, fmt: Format) -> Result<()> {
    let cfg = Config::load()?;
    let client = QueryClient::new(&cfg)?;
    let rows = runtime()?.block_on(client.traces(&filters))?;

    if let Format::Json = fmt {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let mut table = Table::new();
    table.set_header(vec![
        "trace_id",
        "session",
        "harness",
        "last_activity",
        "spans",
        "errors",
    ]);
    for r in &rows {
        table.add_row(vec![
            Cell::new(&r.trace_id),
            Cell::new(r.session_id.as_deref().unwrap_or("-")),
            Cell::new(r.harness.as_deref().unwrap_or("-")),
            Cell::new(r.last_activity.as_deref().unwrap_or("-")),
            Cell::new(r.span_count),
            Cell::new(r.error_count.unwrap_or(0)),
        ]);
    }
    println!("{table}");
    Ok(())
}

pub fn trace(trace_id: String, fmt: TraceFormat) -> Result<()> {
    let cfg = Config::load()?;
    let client = QueryClient::new(&cfg)?;
    let rows = runtime()?.block_on(client.trace(&trace_id))?;

    match fmt {
        TraceFormat::Json => println!("{}", serde_json::to_string_pretty(&rows)?),
        TraceFormat::Tree => print_tree(&rows),
        TraceFormat::Waterfall => print!("{}", crate::waterfall::render(&rows)?),
    }
    Ok(())
}

pub fn waterfall() -> Result<()> {
    let rows: Vec<SpanRow> =
        serde_json::from_reader(std::io::stdin().lock()).context("read span array from stdin")?;
    print!("{}", crate::waterfall::render(&rows)?);
    Ok(())
}

pub fn spans(filters: SpanFilters, fmt: Format) -> Result<()> {
    let cfg = Config::load()?;
    let client = QueryClient::new(&cfg)?;
    let rows = runtime()?.block_on(client.spans(&filters))?;

    if let Format::Json = fmt {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let mut table = Table::new();
    table.set_header(vec!["span_id", "name", "tool", "status", "start_unix_nano"]);
    for r in &rows {
        table.add_row(vec![
            Cell::new(&r.span_id),
            Cell::new(&r.name),
            Cell::new(r.tool_name.as_deref().unwrap_or("-")),
            Cell::new(status_label(r.status)),
            Cell::new(&r.start_unix_nano),
        ]);
    }
    println!("{table}");
    Ok(())
}

pub fn stats(fmt: Format) -> Result<()> {
    let cfg = Config::load()?;
    let client = QueryClient::new(&cfg)?;
    let rows = runtime()?.block_on(client.stats())?;

    if let Format::Json = fmt {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let mut table = Table::new();
    table.set_header(vec!["tool", "spans", "errors", "avg_ms"]);
    for r in &rows {
        table.add_row(vec![
            Cell::new(r.tool_name.as_deref().unwrap_or("-")),
            Cell::new(r.span_count),
            Cell::new(r.error_count.unwrap_or(0)),
            Cell::new(
                r.avg_duration_ms
                    .map(|v| format!("{v:.1}"))
                    .unwrap_or_else(|| "-".into()),
            ),
        ]);
    }
    println!("{table}");
    Ok(())
}

fn status_label(code: i64) -> &'static str {
    match code {
        1 => "ok",
        2 => "error",
        _ => "unset",
    }
}

/// Resolve the live session running in a tmux pane: the most recent span whose
/// `gently.tmux_pane` resource attribute equals `pane`. Because the live session
/// re-stamps this on every hook event, the newest match is authoritative and
/// survives `--resume`/compaction (which strip the id from argv and stale a
/// one-shot `@claude_sid`). Table form prints just the session id (nothing when
/// unknown, so a shell caller can fall back); JSON adds transcript path, cwd and
/// trace id so a consumer can read the transcript and pull the trace.
pub fn whoami(pane: String, fmt: Format) -> Result<()> {
    let cfg = Config::load()?;
    let client = QueryClient::new(&cfg)?;
    // A pane runs one harness at a time; a generous recent window is plenty to
    // catch the newest span it emitted. Filtering is client-side because the
    // pane lives in the resource-attr blob, not a collector-indexed column.
    let rows = runtime()?.block_on(client.spans(&SpanFilters {
        limit: Some(1000),
        ..Default::default()
    }))?;
    let best = rows
        .into_iter()
        .filter(|r| r.resource_attr("gently.tmux_pane").as_deref() == Some(pane.as_str()))
        .max_by_key(|r| r.start_unix_nano.parse::<u128>().unwrap_or(0));

    match fmt {
        Format::Json => {
            let body = match &best {
                Some(r) => serde_json::json!({
                    "tmux_pane": pane,
                    "session_id": r.session_id,
                    "transcript_path": r.resource_attr("gently.transcript_path"),
                    "cwd": r.resource_attr("gently.cwd"),
                    "trace_id": r.trace_id,
                }),
                None => serde_json::json!({ "tmux_pane": pane, "session_id": null }),
            };
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        Format::Table => {
            if let Some(sid) = best.and_then(|r| r.session_id) {
                println!("{sid}");
            }
        }
    }
    Ok(())
}

/// Render the spans of one trace as an indented tree by walking parent links.
fn print_tree(rows: &[SpanRow]) {
    // children keyed by parent span id; roots have no (or unknown) parent.
    let ids: std::collections::HashSet<&str> = rows.iter().map(|r| r.span_id.as_str()).collect();
    let mut children: HashMap<Option<&str>, Vec<&SpanRow>> = HashMap::new();
    for r in rows {
        let parent = r.parent_span_id.as_deref().filter(|p| ids.contains(p));
        children.entry(parent).or_default().push(r);
    }
    fn walk(parent: Option<&str>, children: &HashMap<Option<&str>, Vec<&SpanRow>>, depth: usize) {
        if let Some(kids) = children.get(&parent) {
            for r in kids {
                let dur = span_duration_ms(r);
                println!(
                    "{}{} [{}] {}{}",
                    "  ".repeat(depth),
                    r.name,
                    status_label(r.status),
                    dur,
                    r.tool_name
                        .as_deref()
                        .map(|t| format!(" ({t})"))
                        .unwrap_or_default()
                );
                walk(Some(&r.span_id), children, depth + 1);
            }
        }
    }
    walk(None, &children, 0);
}

fn span_duration_ms(r: &SpanRow) -> String {
    match (&r.end_unix_nano, r.start_unix_nano.parse::<u128>()) {
        (Some(end), Ok(start)) => match end.parse::<u128>() {
            Ok(e) if e >= start => format!("{:.1}ms", (e - start) as f64 / 1e6),
            _ => "-".into(),
        },
        _ => "-".into(),
    }
}
