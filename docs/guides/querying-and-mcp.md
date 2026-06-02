# Querying & MCP

The collector exposes one read surface (`GET /v1/query`). The CLI and the MCP
server are two front-ends onto it.

## From the CLI

```bash
gently traces [--limit N] [--harness claude-code] [--session-id ..] [--since <nanos>] [--until <nanos>] [--order start_desc|start_asc] [--json]
gently trace <trace_id> [--json]          # span tree (indented) or raw spans
gently spans [--trace-id ..] [--session-id ..] [--harness ..] [--tool-name ..] [--name ..] [--status 0|1|2] [--kind ..] [--since <nanos>] [--until <nanos>] [--limit N] [--order start_desc|start_asc] [--json]
gently stats [--json]                     # per-tool counts, errors, avg duration
gently status                             # local exporter health + queue depth
```

`--json` on any query prints raw JSON for piping.

By default, query JSON contains only the collector's digest attributes. Set
`GENTLY_RESOLVE_LOCAL_SHA_RAW_VALUES=1` on the CLI or MCP server process to add
matching local-only raw attributes such as `gently.tool_input`,
`gently.tool_response`, `gently.prompt`, and `gently.assistant`.

### Waterfall

`scripts/waterfall.py` reads `gently trace --json` and renders a depth-indented,
time-proportional waterfall plus integrity checks (root present, parent links
resolved, no negative durations, child-within-parent nesting):

```bash
gently trace <trace_id> --json | python3 scripts/waterfall.py
```

## From the agent (MCP)

`gently mcp` is a stdio MCP server exposing the same surface as tools the agent
can call to introspect its own runs. `gently init` registers it in
`~/.claude.json`; verify with `claude mcp get gently`.

| Tool | Args | Returns |
|---|---|---|
| `list_traces` | `limit?`, `harness?`, `session_id?`, `since?`, `until?`, `order?` | sessions/traces, newest first by default |
| `sessions` | same as `list_traces` | alias for `list_traces` |
| `get_trace` | `trace_id` | all spans for a trace (tree reconstruction) |
| `search_spans` | `trace_id?`, `session_id?`, `harness?`, `tool_name?`, `name?`, `status?`, `kind?`, `since?`, `until?`, `limit?`, `order?` | filtered spans |
| `trace_stats` | - | per-tool rollups |

All tools are read-only. Because the MCP call is itself a tool use, it shows up as
its own span - the system traces itself.

> MCP servers load at session start. After `gently init`, restart the session (or
> `claude --continue`) and confirm with `/hooks` and `claude mcp get gently`.
