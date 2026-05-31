//! Integration test for the MCP stdio handshake: initialize + tools/list work
//! without touching the network (a collector URL is configured but unused).

use assert_cmd::Command;

#[test]
fn mcp_initialize_and_tools_list() {
    let dir = tempfile::tempdir().unwrap();
    let input = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
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
    assert!(names.contains(&"get_trace"));
    assert!(names.contains(&"search_spans"));
    assert!(names.contains(&"trace_stats"));
}
