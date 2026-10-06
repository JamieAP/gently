# Querying and MCP

CLI queries and MCP tools read the same collector API. They require a configured
collector URL and a token available to the querying process. A watcher can export
events queued by hooks without tokens, but it does not supply credentials to a
separate CLI or MCP process. See [configuration](../getting-started/configuration.md)
and [local setup](../getting-started/local-collector.md).

`gently status` is a local health check. `gently waterfall` only reads stdin.
Neither needs a collector token.

## Find a session and inspect its tools

```sh
gently traces --harness codex --limit 5 --order last_activity
gently trace TRACE_ID --waterfall
gently spans --trace-id TRACE_ID --tool-name Bash --order start_asc
gently spans --trace-id TRACE_ID --status 2 --kind 3 --json
```

Replace `TRACE_ID` with an ID from the first command. Use `--harness claude-code`
for Claude Code captures. The last command selects error-status tool/client
spans. A failed or interrupted turn can also have status `2`, so a status filter
alone does not mean tool failure.

`gently trace TRACE_ID` shows a span tree by default. Add `--json` for the
span-row array or `--waterfall` for a chart. The two flags cannot be combined.
`gently stats` summarizes all recorded tool spans, without trace or time filters.
See the [CLI reference](../reference/cli.md) for every command and flag.

## Filters, limits, and ordering

| Query | Default | Supported ordering |
| --- | --- | --- |
| Trace list | 50 summaries, newest start first | `start_desc`, `start_asc`, `last_activity`, `last_activity_desc`, `last_activity_asc` |
| Span search | 50 spans, newest start first | `start_desc`, `start_asc` |
| Single trace | All rows, raw start ascending | Fixed order |
| Tool stats | All tool groups, largest span count first | Fixed order |

Trace-list and span-search limits cap at 1,000. Missing or non-positive limits
use the default. There is no cursor or offset pagination. Equal ordering keys do
not have a defined tie-breaker.

Text filters use exact matches. `since` and `until` are inclusive bounds on raw
`start_unix_nano`, supplied as decimal strings. Keep nanosecond values as strings
instead of JavaScript numbers. For trace summaries, these filters select span
rows **before** grouping by trace: counts and bounds then describe only matching
rows, not necessarily the entire session. `last_activity` is the greatest raw
end, or start for a row without an end, among those rows; it is not ingestion time.

Start filtering and ordering use the stored timestamp text. Use the canonical,
equal-length decimal timestamps produced by the hooks; values with different
digit lengths or leading zeros do not have reliable numeric ordering here.

MCP uses string values for `status` and `kind`, such as `"2"` and `"3"`, and
integer values for `limit`. CLI flags use the equivalent text arguments.
Unknown ordering values fall back to descending start order in the Worker.
Unparseable `status` or `kind` filters are ignored, so use numeric codes rather
than labels such as `error`.

## Read attributes from JSON

CLI and MCP span rows include names, IDs, status, times, tool metadata, and two
JSON-encoded strings: `attrs_json` and `resource_json`. Decode these strings as
JSON arrays to inspect OTLP key/value attributes. A null field means no array was
provided. For example, a decoded attribute can look like:

```json
{"key":"gently.event","value":{"stringValue":"PreToolUse"}}
```

`gently trace --json` includes effective bounds derived by the collector. Span
searches do not derive them, so the CLI/MCP fields are null on those rows. The
[Worker reference](../reference/worker.md#query-response-fields) describes the
wire rows and how they differ from the typed CLI/MCP results.

## Waterfall

```sh
gently trace TRACE_ID --waterfall
gently trace TRACE_ID --json > trace.json
gently waterfall < trace.json
```

The saved-file renderer is native to the Rust binary and does not load local
configuration. The chart uses effective bounds when present, raw bounds as a
fallback, and raw start times to order roots and siblings. Unicode labels stay
aligned; clipped labels end in an ellipsis. The legend identifies `✓` OK, `·`
unset, `✗` error, and `?` unknown.

The report checks roots, parent links, negative raw durations, and nesting with
2 ms of slack. A provisional or unclosed parent has no enforced upper bound.
These checks are diagnostics, not a completeness test for capture. Failed
checks do not make the command fail; invalid JSON or timestamps, an empty array,
duplicate span IDs, and parent cycles do.

## Register MCP with an agent

```sh
gently init --claude
# Or:
gently init --codex
```

Restart the agent to load its registration. Claude Code registration can be
checked with `claude mcp get gently`. Codex hooks also need to be trusted through
`/hooks` inside Codex.

The MCP server uses stdio and read-only tools. It needs either collector
credentials or, on Unix, an unlocked `gently export --watch --serve-queries --preserve-backlog`
using the same state directory, tenant, device and collector URL. Tokenless desktop clients
automatically delegate queries through the private socket. Restart older
watchers to enable this; the bundled local launchers pass the flag.

| Tool | Arguments | Result |
| --- | --- | --- |
| `list_traces` | `limit?`, `harness?`, `session_id?`, `since?`, `until?`, `order?`, `jq?` | Trace summaries |
| `sessions` | Same as `list_traces` | Alias for `list_traces` |
| `get_trace` | `trace_id`, `jq?` | All rows for one trace |
| `search_spans` | `trace_id?`, `session_id?`, `harness?`, `tool_name?`, `name?`, `status?`, `kind?`, `since?`, `until?`, `limit?`, `order?`, `jq?` | Matching span rows |
| `trace_stats` | `jq?` | Per-tool counts, errors, and mean duration |
| `response_fields` | `tool?`, `jq?` | Common response fields and filter examples |
| `span_attr_keys` | Same filters as `search_spans`, plus `jq?` | `span_attrs`, `resource_attrs`, and `spans_scanned` |

`response_fields` does not fetch collector data, although server startup still
requires credentials or a local query socket. `span_attr_keys` inspects only the returned search window;
it is not an inventory of every attribute ever captured. When the harness emits
tool hooks for MCP calls, a Gently query can itself appear as a tool span.

### Example tool calls

These JSON objects are the `params` of MCP `tools/call` requests. First select
recent sessions and keep only the fields needed to choose a trace:

```json
{
  "name": "list_traces",
  "arguments": {
    "harness": "codex",
    "limit": 5,
    "order": "last_activity",
    "jq": "map({trace_id, session_id, last_activity})"
  }
}
```

Then inspect failed tool spans in the selected trace:

```json
{
  "name": "search_spans",
  "arguments": {
    "trace_id": "TRACE_ID",
    "status": "2",
    "kind": "3",
    "limit": 100,
    "order": "start_asc",
    "jq": "map({name, tool_name, start_unix_nano})"
  }
}
```

Use `response_fields` with `{"tool":"get_trace"}` to discover common row fields,
or `span_attr_keys` with `{"trace_id":"TRACE_ID","limit":1000}` to inspect
available attribute names.

### Local `jq` filters

The `jq` argument is evaluated by the embedded Rust `jaq` implementation after
the response is fetched and after optional reader-side raw-value resolution. It is
never sent to the Worker and does not reduce the collector query's row limit.
It needs no installed `jq` executable and is not a shell command. Environment
access through `env` is unavailable; full compatibility with every standalone
`jq` feature is not promised.

No filter or a blank filter returns the payload unchanged. Zero emitted values
become `[]`; one becomes that JSON value; multiple become a JSON array. MCP wraps
the resulting JSON as pretty-printed text in a `content` block. Filter parse,
compile, or execution errors return a tool-call error.

## Raw values

Normal exports contain metadata and byte lengths, with optional random
`.raw_ref` pointers. Capture, ciphertext cloud sync and reader resolution are
separate opt-ins, disabled by default. No public raw-content hashes are emitted.

Follow [encrypted raw enrollment](encrypted-raw-values.md) before enabling
`capture_raw_values` on hooks. Capture uses only signed, locally pinned public
recipient policy. Exporters can sync ciphertext without a reader identity.

An enrolled reader sets `resolve_raw_values = true` or
`GENTLY_RESOLVE_RAW_VALUES=1` and `raw_identity`/`GENTLY_RAW_IDENTITY`. It uses
local ciphertext first, fetching absent objects through the tenant-authorized
collector when necessary. Resolution explicitly unlocks the identity, verifies
encrypted context and field-to-span bindings, and adds raw attributes only to
the in-memory result. It does not enable capture or cloud sync.

Software identities need an attached private terminal for passphrase entry.
A GUI MCP process without a terminal cannot unlock one; start its reader process
interactively or use an enrolled Mac hardware reader. No passphrase environment
variable or background unlock broker is provided.

Install MCP with `--resolve-raw-values` to set its registration opt-in.
Reinstalling without that flag removes the registration setting; independent
configuration/environment settings can still enable it. Decrypted output can
enter the calling agent's model-provider context. Retained ciphertext remains
after capture is disabled, and new readers cannot automatically decrypt older
objects. See [security and privacy](../concepts/security-and-privacy.md).
