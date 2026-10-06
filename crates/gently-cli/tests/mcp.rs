//! Integration test for the MCP stdio handshake: initialize + tools/list work
//! without touching the network (a collector URL is configured but unused).

use assert_cmd::Command;

#[test]
fn mcp_initialize_and_tools_list() {
    let dir = tempfile::tempdir().unwrap();
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"synthetic-client","version":"1"},"_meta":{"$serde_json::private::Number":"ordinary metadata"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        "\n",
    );

    let assert = Command::cargo_bin("gently")
        .unwrap()
        .arg("mcp")
        .env("GENTLY_STATE_DIR", dir.path())
        .env("GENTLY_COLLECTOR_URL", "http://127.0.0.1:9")
        .env("GENTLY_TOKEN", "t")
        .write_stdin(input)
        .assert()
        .success();

    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "one response per request");

    let init: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "gently");

    let tools: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"list_traces"));
    assert!(names.contains(&"sessions"));
    assert!(names.contains(&"get_trace"));
    assert!(names.contains(&"search_spans"));
    assert!(names.contains(&"trace_stats"));
    assert!(names.contains(&"response_fields"));
    assert!(names.contains(&"span_attr_keys"));

    let sessions = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "sessions")
        .unwrap();
    let order_enum = sessions["inputSchema"]["properties"]["order"]["enum"]
        .as_array()
        .unwrap();
    assert!(order_enum.iter().any(|v| v == "last_activity"));
    assert!(sessions["inputSchema"]["properties"].get("jq").is_some());
}

#[test]
fn mcp_response_fields_accepts_jq_filter() {
    let dir = tempfile::tempdir().unwrap();
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"synthetic-client","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"response_fields","arguments":{"tool":"sessions","jq":".fields"}}}"#,
        "\n",
    );

    let assert = Command::cargo_bin("gently")
        .unwrap()
        .arg("mcp")
        .env("GENTLY_STATE_DIR", dir.path())
        .env("GENTLY_COLLECTOR_URL", "http://127.0.0.1:9")
        .env("GENTLY_TOKEN", "t")
        .write_stdin(input)
        .assert()
        .success();

    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "one response per request");

    let response: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    let fields: Vec<String> = serde_json::from_str(text).unwrap();
    assert!(fields.iter().any(|f| f == "last_activity"));
}

#[test]
fn mcp_metadata_handshake_does_not_unlock_or_require_an_opted_in_reader() {
    let dir = tempfile::tempdir().unwrap();
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"synthetic-client","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"response_fields","arguments":{}}}"#,
        "\n",
    );
    let assert = Command::cargo_bin("gently")
        .unwrap()
        .arg("mcp")
        .env("GENTLY_STATE_DIR", dir.path())
        .env("GENTLY_TENANT_ID", "personal")
        .env("GENTLY_DEVICE_ID", "synthetic-reader")
        .env("GENTLY_COLLECTOR_URL", "http://127.0.0.1:9")
        .env("GENTLY_TOKEN", "synthetic-auth")
        .env("GENTLY_RESOLVE_RAW_VALUES", "1")
        .env("GENTLY_RAW_IDENTITY", dir.path().join("missing-reader.age"))
        .write_stdin(input)
        .assert()
        .success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let messages: Vec<serde_json::Value> = output
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(messages.len(), 3);
    assert!(messages
        .iter()
        .all(|message| message.get("error").is_none()));
    assert!(assert.get_output().stderr.is_empty());
}

fn run_messages(input: String) -> Vec<serde_json::Value> {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("gently")
        .unwrap()
        .arg("mcp")
        .env("GENTLY_STATE_DIR", dir.path())
        .env("GENTLY_COLLECTOR_URL", "http://127.0.0.1:9")
        .env("GENTLY_TOKEN", "synthetic-token")
        .write_stdin(input)
        .assert()
        .success();
    assert!(output.get_output().stderr.is_empty());
    String::from_utf8(output.get_output().stdout.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn handshake() -> String {
    concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1900-01-01","capabilities":{},"clientInfo":{"name":"synthetic","version":"1"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
    ).into()
}

#[test]
fn negotiation_ping_protocol_errors_and_tool_execution_errors() {
    let mut input = handshake();
    for (id, method, params) in [
        (2, "ping", serde_json::json!({})),
        (3, "unknown", serde_json::json!({})),
        (4, "tools/call", serde_json::json!({"name":"unknown"})),
        (5, "tools/call", serde_json::json!({"name":"get_trace"})),
        (
            6,
            "tools/call",
            serde_json::json!({"name":"search_spans","arguments":{"limit":4294967297u64}}),
        ),
        (
            7,
            "tools/call",
            serde_json::json!({"name":"search_spans","arguments":{"order":"invalid"}}),
        ),
        (
            8,
            "tools/call",
            serde_json::json!({"name":"trace_stats","arguments":{"jq":42}}),
        ),
        (9, "tools/call", serde_json::json!({"name":"trace_stats"})),
        (
            10,
            "tools/call",
            serde_json::json!({"name":"response_fields","arguments":{"jq":"["}}),
        ),
    ] {
        input += &format!(
            "{}\n",
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        );
    }
    let rows = run_messages(input);
    assert_eq!(rows[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(rows[1]["result"], serde_json::json!({}));
    assert_eq!(rows[2]["error"]["code"], -32601);
    for row in &rows[3..8] {
        assert_eq!(row["error"]["code"], -32602);
    }
    for row in &rows[8..] {
        assert_eq!(row["result"]["isError"], true);
        assert!(row.get("error").is_none());
        assert!(!row.to_string().contains("synthetic-token"));
    }
}

#[test]
fn invalid_and_oversized_frames_recover_without_replying_to_notifications() {
    let mut input = String::from("invalid JSON\n[]\n{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n");
    input += &"x".repeat(1024 * 1024 + 1);
    input += "\n";
    input += &handshake();
    let rows = run_messages(input);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0]["error"]["code"], -32700);
    assert_eq!(rows[1]["error"]["code"], -32600);
    assert_eq!(rows[2]["error"]["code"], -32700);
    assert_eq!(rows[3]["result"]["serverInfo"]["name"], "gently");
}

#[test]
fn schema_valid_empty_jq_is_a_noop() {
    let input = handshake() + "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"response_fields\",\"arguments\":{\"tool\":\"sessions\",\"jq\":\"\"}}}\n";
    let rows = run_messages(input);
    assert_eq!(rows[1]["result"]["isError"], false);
}

#[test]
fn invalid_envelopes_preserve_detected_integer_or_string_ids() {
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":42,"method":false}"#,
        "\n",
        r#"{"jsonrpc":"wrong","id":"correlate","method":"ping"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#,
        "\n",
    );
    let rows = run_messages(input.into());
    assert_eq!(rows[0]["id"], 42);
    assert_eq!(rows[1]["id"], "correlate");
    assert!(rows[2]["id"].is_null());
    assert!(rows.iter().all(|row| row["error"]["code"] == -32600));
}
