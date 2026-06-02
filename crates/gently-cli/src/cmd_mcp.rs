//! `gently mcp` - a stdio MCP server exposing the collector's read surface as
//! tools the harness can call to introspect its own traces.
//!
//! Implements the Model Context Protocol JSON-RPC 2.0 stdio transport directly
//! (newline-delimited messages). Hand-rolling the small, well-specified handshake
//! keeps the dependency surface minimal and avoids SDK version churn. All tools
//! are read-only.

use crate::config::Config;
use crate::query_client::{QueryClient, SpanFilters, TraceFilters};
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2025-06-18";

pub fn run() -> Result<()> {
    let cfg = Config::load()?;
    let client = QueryClient::new(&cfg)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Notifications have no id and expect no response.
        let Some(id) = req.get("id").cloned() else {
            continue;
        };
        let method = req
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        let response = match handle(method, &params, &client, &runtime) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32000, "message": e.to_string()}
            }),
        };
        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }
    Ok(())
}

fn handle(
    method: &str,
    params: &Value,
    client: &QueryClient,
    rt: &tokio::runtime::Runtime,
) -> Result<Value> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": params.get("protocolVersion")
                .and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "gently", "version": env!("CARGO_PKG_VERSION")},
        })),
        "tools/list" => Ok(json!({"tools": tool_specs()})),
        "tools/call" => call_tool(params, client, rt),
        other => anyhow::bail!("unknown method: {other}"),
    }
}

fn call_tool(params: &Value, client: &QueryClient, rt: &tokio::runtime::Runtime) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let payload: Value = match name {
        "list_traces" | "sessions" => {
            let f = TraceFilters {
                limit: args.get("limit").and_then(Value::as_u64).map(|v| v as u32),
                harness: str_arg(&args, "harness"),
                session_id: str_arg(&args, "session_id"),
                since: str_arg(&args, "since"),
                until: str_arg(&args, "until"),
                order: str_arg(&args, "order"),
            };
            serde_json::to_value(rt.block_on(client.traces(&f))?)?
        }
        "get_trace" => {
            let trace_id = args
                .get("trace_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            serde_json::to_value(rt.block_on(client.trace(trace_id))?)?
        }
        "search_spans" => {
            let f = SpanFilters {
                trace_id: str_arg(&args, "trace_id"),
                session_id: str_arg(&args, "session_id"),
                harness: str_arg(&args, "harness"),
                tool_name: str_arg(&args, "tool_name"),
                name: str_arg(&args, "name"),
                status: str_arg(&args, "status"),
                kind: str_arg(&args, "kind"),
                since: str_arg(&args, "since"),
                until: str_arg(&args, "until"),
                limit: args.get("limit").and_then(Value::as_u64).map(|v| v as u32),
                order: str_arg(&args, "order"),
            };
            serde_json::to_value(rt.block_on(client.spans(&f))?)?
        }
        "trace_stats" => serde_json::to_value(rt.block_on(client.stats())?)?,
        other => anyhow::bail!("unknown tool: {other}"),
    };

    // MCP tool results return content blocks; embed the JSON as text.
    Ok(json!({
        "content": [{"type": "text", "text": serde_json::to_string_pretty(&payload)?}]
    }))
}

fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

fn tool_specs() -> Value {
    json!([
        {
            "name": "list_traces",
            "description": "List harness sessions/traces, newest first by default.",
            "inputSchema": {"type": "object", "properties": {
                "limit": {"type": "integer", "description": "max traces"},
                "harness": {"type": "string", "description": "filter by harness, e.g. claude-code"},
                "session_id": {"type": "string", "description": "filter by exact session id"},
                "since": {"type": "string", "description": "minimum start_unix_nano"},
                "until": {"type": "string", "description": "maximum start_unix_nano"},
                "order": {"type": "string", "enum": ["start_desc", "start_asc"], "description": "sort order"}
            }}
        },
        {
            "name": "sessions",
            "description": "Alias for list_traces; returns harness sessions/traces.",
            "inputSchema": {"type": "object", "properties": {
                "limit": {"type": "integer", "description": "max sessions"},
                "harness": {"type": "string", "description": "filter by harness, e.g. claude-code"},
                "session_id": {"type": "string", "description": "filter by exact session id"},
                "since": {"type": "string", "description": "minimum start_unix_nano"},
                "until": {"type": "string", "description": "maximum start_unix_nano"},
                "order": {"type": "string", "enum": ["start_desc", "start_asc"], "description": "sort order"}
            }}
        },
        {
            "name": "get_trace",
            "description": "Get all spans for a trace id (for tree reconstruction).",
            "inputSchema": {"type": "object", "required": ["trace_id"], "properties": {
                "trace_id": {"type": "string"}
            }}
        },
        {
            "name": "search_spans",
            "description": "Search spans by indexed columns; newest first by default.",
            "inputSchema": {"type": "object", "properties": {
                "trace_id": {"type": "string"},
                "session_id": {"type": "string"},
                "harness": {"type": "string", "description": "filter by harness, e.g. codex"},
                "tool_name": {"type": "string"},
                "name": {"type": "string", "description": "span name"},
                "status": {"type": "string", "description": "OTLP status code 0/1/2"},
                "kind": {"type": "string", "description": "OTLP span kind code"},
                "since": {"type": "string", "description": "start_unix_nano lower bound"},
                "until": {"type": "string", "description": "start_unix_nano upper bound"},
                "limit": {"type": "integer"},
                "order": {"type": "string", "enum": ["start_desc", "start_asc"], "description": "sort order"}
            }}
        },
        {
            "name": "trace_stats",
            "description": "Per-tool rollups: span counts, error counts, average duration.",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ])
}
