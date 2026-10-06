//! `gently mcp` - a stdio MCP server exposing the collector's read surface as
//! tools the harness can call to introspect its own traces.
//!
//! Implements the Model Context Protocol JSON-RPC 2.0 stdio transport directly
//! (newline-delimited messages). Hand-rolling the small, well-specified handshake
//! keeps the dependency surface minimal and avoids SDK version churn. All tools
//! are read-only.

use crate::config::Config;
use crate::mcp_jq;
use crate::query_client::{QueryClient, SpanFilters, TraceFilters};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::BTreeSet;
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
        let req: Value = match crate::json_fidelity::parse(&line) {
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

    let jq = str_arg(&args, "jq");
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
        "response_fields" => response_fields(str_arg(&args, "tool").as_deref())?,
        "span_attr_keys" => {
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
            let rows = rt.block_on(client.spans(&f))?;
            span_attr_keys(&rows)
        }
        other => anyhow::bail!("unknown tool: {other}"),
    };
    let payload = mcp_jq::apply(payload, jq.as_deref())?;

    // MCP tool results return content blocks; embed the JSON as text.
    Ok(json!({
        "content": [{"type": "text", "text": serde_json::to_string_pretty(&payload)?}]
    }))
}

fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

fn response_fields(tool: Option<&str>) -> Result<Value> {
    let all = json!({
        "list_traces": {
            "fields": ["trace_id", "session_id", "harness", "start", "last_activity", "span_count", "error_count"],
            "jq_examples": [
                "map({session_id, harness, last_activity})",
                ".[] | {trace_id, last_activity}"
            ]
        },
        "sessions": {
            "fields": ["trace_id", "session_id", "harness", "start", "last_activity", "span_count", "error_count"],
            "jq_examples": [
                "map({session_id, harness, last_activity})",
                ".[] | {trace_id, last_activity}"
            ]
        },
        "get_trace": {
            "fields": ["span_id", "trace_id", "parent_span_id", "name", "kind", "start_unix_nano", "end_unix_nano", "status", "session_id", "harness", "tool_name", "tool_use_id", "resource_json", "attrs_json"],
            "notes": ["attrs_json and resource_json are JSON-encoded OTLP key/value arrays"],
            "jq_examples": [
                "map({name, tool_name, status})",
                "map(select(.tool_name == \"Bash\") | {name, start_unix_nano})"
            ]
        },
        "search_spans": {
            "fields": ["span_id", "trace_id", "parent_span_id", "name", "kind", "start_unix_nano", "end_unix_nano", "status", "session_id", "harness", "tool_name", "tool_use_id", "resource_json", "attrs_json"],
            "notes": ["attrs_json and resource_json are JSON-encoded OTLP key/value arrays"],
            "jq_examples": [
                "map({name, tool_name, status})",
                "map(select(.tool_name == \"Bash\") | {name, start_unix_nano})"
            ]
        },
        "trace_stats": {
            "fields": ["tool_name", "span_count", "error_count", "avg_duration_ms"],
            "jq_examples": [
                "map(select(.error_count > 0) | {tool_name, error_count})"
            ]
        },
        "span_attr_keys": {
            "fields": ["span_attrs", "resource_attrs", "spans_scanned"],
            "jq_examples": [
                "{span_attrs, resource_attrs}"
            ]
        }
    });

    match tool {
        Some(name) => all
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown response field target: {name}")),
        None => Ok(all),
    }
}

fn span_attr_keys(rows: &[crate::query_client::SpanRow]) -> Value {
    let mut span_attrs = BTreeSet::new();
    let mut resource_attrs = BTreeSet::new();
    for row in rows {
        collect_attr_keys(row.attrs_json.as_deref(), &mut span_attrs);
        collect_attr_keys(row.resource_json.as_deref(), &mut resource_attrs);
    }
    json!({
        "span_attrs": span_attrs.into_iter().collect::<Vec<_>>(),
        "resource_attrs": resource_attrs.into_iter().collect::<Vec<_>>(),
        "spans_scanned": rows.len(),
    })
}

fn collect_attr_keys(blob: Option<&str>, keys: &mut BTreeSet<String>) {
    let Some(blob) = blob else { return };
    let Ok(attrs) = serde_json::from_str::<Value>(blob) else {
        return;
    };
    let Some(attrs) = attrs.as_array() else {
        return;
    };
    keys.extend(
        attrs
            .iter()
            .filter_map(|kv| kv.get("key").and_then(Value::as_str).map(str::to_string)),
    );
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
                "order": {"type": "string", "enum": ["start_desc", "start_asc", "last_activity", "last_activity_desc", "last_activity_asc"], "description": "sort order"},
                "jq": {"type": "string", "description": "local jq filter applied to this tool's JSON result before returning"}
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
                "order": {"type": "string", "enum": ["start_desc", "start_asc", "last_activity", "last_activity_desc", "last_activity_asc"], "description": "sort order"},
                "jq": {"type": "string", "description": "local jq filter applied to this tool's JSON result before returning"}
            }}
        },
        {
            "name": "get_trace",
            "description": "Get all spans for a trace id (for tree reconstruction).",
            "inputSchema": {"type": "object", "required": ["trace_id"], "properties": {
                "trace_id": {"type": "string"},
                "jq": {"type": "string", "description": "local jq filter applied to this tool's JSON result before returning"}
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
                "order": {"type": "string", "enum": ["start_desc", "start_asc"], "description": "sort order"},
                "jq": {"type": "string", "description": "local jq filter applied to this tool's JSON result before returning"}
            }}
        },
        {
            "name": "trace_stats",
            "description": "Per-tool rollups: span counts, error counts, average duration.",
            "inputSchema": {"type": "object", "properties": {
                "jq": {"type": "string", "description": "local jq filter applied to this tool's JSON result before returning"}
            }}
        },
        {
            "name": "response_fields",
            "description": "Describe top-level JSON response fields for gently MCP tools.",
            "inputSchema": {"type": "object", "properties": {
                "tool": {"type": "string", "enum": ["list_traces", "sessions", "get_trace", "search_spans", "trace_stats", "span_attr_keys"]},
                "jq": {"type": "string", "description": "local jq filter applied to the schema result before returning"}
            }}
        },
        {
            "name": "span_attr_keys",
            "description": "Discover span/resource attribute keys across recent matching spans.",
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
                "limit": {"type": "integer", "description": "max spans to inspect"},
                "order": {"type": "string", "enum": ["start_desc", "start_asc"], "description": "sort order"},
                "jq": {"type": "string", "description": "local jq filter applied to the discovered keys before returning"}
            }}
        }
    ])
}
