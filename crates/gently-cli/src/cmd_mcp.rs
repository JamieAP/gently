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
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::{BufRead, Write};

const MAX_FRAME_BYTES: usize = 1024 * 1024;

const PROTOCOL_VERSION: &str = "2025-06-18";

pub fn run() -> Result<()> {
    let cfg = Config::load()?;
    let client = QueryClient::new(&cfg)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    serve(stdin_lock(), &mut std::io::stdout(), &client, &runtime)
}

fn stdin_lock() -> std::io::StdinLock<'static> {
    std::io::stdin().lock()
}

#[derive(Debug)]
struct RpcError(i32, String);
fn rpc_failure(code: i32, message: impl Into<String>) -> RpcError {
    RpcError(code, message.into())
}

#[derive(Debug, thiserror::Error)]
enum ToolFailure {
    #[error("Local jq filter failed; correct the filter syntax or operation.")]
    Filter,
}
fn tool_failure(error: &anyhow::Error) -> String {
    if let Some(error) = error.downcast_ref::<ToolFailure>() {
        return error.to_string();
    }
    if let Some(error) = error.downcast_ref::<crate::query_client::QueryFailure>() {
        return error.to_string();
    }
    "Collector query failed; check collector availability, authorization and query arguments."
        .into()
}

fn valid_id(id: &Value) -> bool {
    id.is_string() || id.as_i64().is_some() || id.as_u64().is_some()
}

fn rpc_error(id: Value, error: RpcError) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": error.0, "message": error.1}})
}

// Read bounded frames and drain an oversized frame without allocating its tail.
fn frame(input: &mut impl BufRead) -> std::io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    let mut oversized = false;
    loop {
        let chunk = input.fill_buf()?;
        if chunk.is_empty() {
            return Ok((!bytes.is_empty() || oversized).then_some(bytes));
        }
        let length = chunk
            .iter()
            .position(|&v| v == b'\n')
            .map_or(chunk.len(), |i| i + 1);
        let finished = chunk[length - 1] == b'\n';
        if !oversized && bytes.len() + length <= MAX_FRAME_BYTES {
            bytes.extend_from_slice(&chunk[..length]);
        } else {
            oversized = true;
            bytes.clear();
        }
        input.consume(length);
        if finished {
            // An empty sentinel is an invalid/oversized frame, never a notification.
            return Ok(Some(bytes));
        }
    }
}

fn serve(
    mut input: impl BufRead,
    output: &mut impl Write,
    client: &QueryClient,
    runtime: &tokio::runtime::Runtime,
) -> Result<()> {
    let mut initialized = false;
    let mut negotiated = false;
    while let Some(bytes) = frame(&mut input)? {
        if !bytes.is_empty() && bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let response = match crate::json_fidelity::parse_bytes(&bytes) {
            Err(_) => Some(rpc_error(
                Value::Null,
                rpc_failure(-32700, "Invalid or oversized JSON frame"),
            )),
            Ok(req) => {
                let id = req.get("id").cloned().unwrap_or(Value::Null);
                if !req.is_object()
                    || req.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
                    || req.get("method").and_then(Value::as_str).is_none()
                    || (req.get("id").is_some() && !valid_id(&id))
                {
                    Some(rpc_error(
                        if valid_id(&id) { id } else { Value::Null },
                        rpc_failure(-32600, "Invalid JSON-RPC request"),
                    ))
                } else {
                    let method = req["method"].as_str().unwrap();
                    let params = req.get("params").cloned().unwrap_or(json!({}));
                    if req.get("id").is_none() {
                        if method == "notifications/initialized" && negotiated {
                            initialized = true;
                        }
                        None
                    } else {
                        let result = if method == "initialize" && negotiated {
                            Err(rpc_failure(-32600, "Already initialized"))
                        } else if !initialized && !matches!(method, "initialize" | "ping") {
                            Err(rpc_failure(-32600, "Initialize the MCP session first"))
                        } else {
                            handle(method, &params, client, runtime)
                        };
                        match result {
                            Ok(result) => {
                                if method == "initialize" {
                                    negotiated = true;
                                }
                                Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
                            }
                            Err(error) => Some(rpc_error(id, error)),
                        }
                    }
                }
            }
        };
        if let Some(response) = response {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }
    Ok(())
}

fn handle(
    method: &str,
    params: &Value,
    client: &QueryClient,
    rt: &tokio::runtime::Runtime,
) -> std::result::Result<Value, RpcError> {
    if !params.is_object() {
        return Err(rpc_failure(-32602, "Expected object parameters"));
    }
    match method {
        "initialize" => {
            if params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .is_none()
                || !params.get("capabilities").is_some_and(Value::is_object)
                || params
                    .get("clientInfo")
                    .and_then(|v| v.get("name"))
                    .and_then(Value::as_str)
                    .is_none()
                || params
                    .get("clientInfo")
                    .and_then(|v| v.get("version"))
                    .and_then(Value::as_str)
                    .is_none()
            {
                return Err(rpc_failure(-32602, "Missing initialization fields"));
            }
            Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "gently", "version": env!("CARGO_PKG_VERSION")},
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" if params.get("cursor").is_some() => {
            Err(rpc_failure(-32602, "No tool-list cursor is available"))
        }
        "tools/list" => Ok(json!({"tools": tool_specs()})),
        "tools/call" => {
            validate_tool(params)?;
            // Execution errors belong in CallToolResult, not JSON-RPC errors. Do
            // not expose collector URLs, credentials, payloads or filter source.
            Ok(call_tool(params, client, rt).unwrap_or_else(|error| {
                json!({
                    "isError": true,
                    "content": [{"type": "text", "text": tool_failure(&error)}]
                })
            }))
        }
        _ => Err(rpc_failure(-32601, "Method not found")),
    }
}

// JSON Schema accepts decimal/exponent spellings of integers. Inspect the
// preserved number before converting bounded limits; f64 can round a fraction.
fn exact_integer(value: &Value) -> bool {
    let Some(number) = value.as_number() else {
        return false;
    };
    let text = number.to_string();
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => {
            let Ok(exponent) = exponent.parse::<i64>() else {
                return false;
            };
            (mantissa, exponent)
        }
        None => (text.as_str(), 0),
    };
    let fraction = mantissa
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let Some(scale) = exponent.checked_sub(fraction as i64) else {
        return false;
    };
    if scale >= 0 {
        return true;
    }
    let digits = mantissa.bytes().filter(u8::is_ascii_digit).rev();
    digits
        .take(scale.unsigned_abs().min(usize::MAX as u64) as usize)
        .all(|digit| digit == b'0')
}

fn validate_tool(params: &Value) -> std::result::Result<(), RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or(rpc_failure(-32602, "Missing tool name"))?;
    let specs = tool_specs();
    let spec = specs
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .ok_or(rpc_failure(-32602, "Unknown tool"))?;
    let empty = json!({});
    let args = params
        .get("arguments")
        .unwrap_or(&empty)
        .as_object()
        .ok_or(rpc_failure(-32602, "Expected object arguments"))?;
    let schema = &spec["inputSchema"];
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required {
            if !args.contains_key(key.as_str().unwrap()) {
                return Err(rpc_failure(-32602, "Missing required tool argument"));
            }
        }
    }
    for (key, value) in args {
        let property = schema["properties"]
            .get(key)
            .ok_or(rpc_failure(-32602, "Unknown tool argument"))?;
        let valid = match property["type"].as_str() {
            Some("string") => value.as_str().is_some(),
            Some("integer") => value.as_f64().is_some_and(|v| {
                v.is_finite()
                    && exact_integer(value)
                    && property
                        .get("minimum")
                        .and_then(Value::as_f64)
                        .is_none_or(|min| v >= min)
                    && property
                        .get("maximum")
                        .and_then(Value::as_f64)
                        .is_none_or(|max| v <= max)
            }),
            _ => return Err(rpc_failure(-32603, "Unsupported tool schema")),
        };
        if !valid
            || property
                .get("enum")
                .and_then(Value::as_array)
                .is_some_and(|v| !v.contains(value))
        {
            let expected = match property["type"].as_str() {
                Some("integer") => format!(
                    "integer from {} to {}",
                    property["minimum"], property["maximum"]
                ),
                _ if property.get("enum").is_some() => format!("one of {}", property["enum"]),
                _ => "string".into(),
            };
            return Err(rpc_failure(
                -32602,
                format!("Invalid {key}: expected {expected}"),
            ));
        }
    }
    Ok(())
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
                limit: args.get("limit").and_then(Value::as_f64).map(|v| v as u32),
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
                limit: args.get("limit").and_then(Value::as_f64).map(|v| v as u32),
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
                limit: args.get("limit").and_then(Value::as_f64).map(|v| v as u32),
                order: str_arg(&args, "order"),
            };
            let rows = rt.block_on(client.spans(&f))?;
            span_attr_keys(&rows)
        }
        other => anyhow::bail!("unknown tool: {other}"),
    };
    let payload = mcp_jq::apply(payload, jq.as_deref()).context(ToolFailure::Filter)?;

    // MCP tool results return content blocks; embed the JSON as text.
    Ok(json!({
        "isError": false,
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
    let Ok(attrs) = crate::json_fidelity::parse(blob) else {
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
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "description": "max traces"},
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
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "description": "max sessions"},
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
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["trace_id"], "properties": {
                "trace_id": {"type": "string"},
                "jq": {"type": "string", "description": "local jq filter applied to this tool's JSON result before returning"}
            }}
        },
        {
            "name": "search_spans",
            "description": "Search spans by indexed columns; newest first by default.",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "trace_id": {"type": "string"},
                "session_id": {"type": "string"},
                "harness": {"type": "string", "description": "filter by harness, e.g. codex"},
                "tool_name": {"type": "string"},
                "name": {"type": "string", "description": "span name"},
                "status": {"type": "string", "description": "OTLP status code 0/1/2"},
                "kind": {"type": "string", "description": "OTLP span kind code"},
                "since": {"type": "string", "description": "start_unix_nano lower bound"},
                "until": {"type": "string", "description": "start_unix_nano upper bound"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000},
                "order": {"type": "string", "enum": ["start_desc", "start_asc"], "description": "sort order"},
                "jq": {"type": "string", "description": "local jq filter applied to this tool's JSON result before returning"}
            }}
        },
        {
            "name": "trace_stats",
            "description": "Per-tool rollups: span counts, error counts, average duration.",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "jq": {"type": "string", "description": "local jq filter applied to this tool's JSON result before returning"}
            }}
        },
        {
            "name": "response_fields",
            "description": "Describe top-level JSON response fields for gently MCP tools.",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "tool": {"type": "string", "enum": ["list_traces", "sessions", "get_trace", "search_spans", "trace_stats", "span_attr_keys"]},
                "jq": {"type": "string", "description": "local jq filter applied to the schema result before returning"}
            }}
        },
        {
            "name": "span_attr_keys",
            "description": "Discover span/resource attribute keys across recent matching spans.",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "trace_id": {"type": "string"},
                "session_id": {"type": "string"},
                "harness": {"type": "string", "description": "filter by harness, e.g. codex"},
                "tool_name": {"type": "string"},
                "name": {"type": "string", "description": "span name"},
                "status": {"type": "string", "description": "OTLP status code 0/1/2"},
                "kind": {"type": "string", "description": "OTLP span kind code"},
                "since": {"type": "string", "description": "start_unix_nano lower bound"},
                "until": {"type": "string", "description": "start_unix_nano upper bound"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "description": "max spans to inspect"},
                "order": {"type": "string", "enum": ["start_desc", "start_asc"], "description": "sort order"},
                "jq": {"type": "string", "description": "local jq filter applied to the discovered keys before returning"}
            }}
        }
    ])
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    #[test]
    fn shipped_schemas_use_supported_types_and_enforce_their_own_bounds() {
        for spec in tool_specs().as_array().unwrap() {
            for property in spec["inputSchema"]["properties"]
                .as_object()
                .unwrap()
                .values()
            {
                assert!(
                    matches!(property["type"].as_str(), Some("string" | "integer")),
                    "add explicit validation for new schema types"
                );
            }
        }
        for (value, valid) in [
            (json!(1), true),
            (json!(5.0), true),
            (json!(1000), true),
            (json!(0), false),
            (json!(1001), false),
            (json!(1.5), false),
        ] {
            assert_eq!(
                validate_tool(&json!({"name":"search_spans","arguments":{"limit":value}})).is_ok(),
                valid
            );
        }
    }
    #[test]
    fn integer_schema_uses_preserved_decimal_not_rounded_f64() {
        for (number, valid) in [
            ("1.00000000000000001", false),
            ("1000.00000000000000001", false),
            ("5.0", true),
            ("5e0", true),
            ("0.5e1", true),
            ("1e-100", false),
            ("0e-100", false),
        ] {
            let params = crate::json_fidelity::parse(&format!(
                r#"{{"name":"search_spans","arguments":{{"limit":{number}}}}}"#
            ))
            .unwrap();
            assert_eq!(validate_tool(&params).is_ok(), valid, "{number}");
        }
    }
    #[test]
    fn fixed_tool_failures_do_not_echo_error_chains() {
        let private = "synthetic-private-error-canary";
        for error in [
            anyhow::anyhow!(private),
            anyhow::anyhow!(private).context(ToolFailure::Filter),
            anyhow::anyhow!(private).context(crate::query_client::QueryFailure::RawBudget),
            anyhow::anyhow!(private).context(crate::query_client::QueryFailure::ReaderUnavailable),
            anyhow::anyhow!(private).context(crate::query_client::QueryFailure::WatcherUnavailable),
        ] {
            assert!(!tool_failure(&error).contains(private));
        }
    }
}
