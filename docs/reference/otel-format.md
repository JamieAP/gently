# OTLP/JSON format

Gently serializes trace export requests as OTLP/JSON. This page describes the
fields emitted by the current Rust implementation and how the included Worker
stores them. It is not a claim that the Worker validates every OTLP feature.

## Envelope

A request contains `resourceSpans`, each with resource attributes and one or more
`scopeSpans` groups. Each scope is named `gently` and carries the crate version.
The exporter batches requests by concatenating their resource groups.

This synthetic example contains one session root with no parent:

```json
{
  "resourceSpans": [{
    "resource": {
      "attributes": [
        {"key":"service.name","value":{"stringValue":"gently"}},
        {"key":"gently.harness","value":{"stringValue":"codex"}},
        {"key":"gently.session_id","value":{"stringValue":"example-session"}}
      ]
    },
    "scopeSpans": [{
      "scope": {"name":"gently","version":"0.1.0"},
      "spans": [{
        "traceId":"11111111111111111111111111111111",
        "spanId":"2222222222222222",
        "name":"session",
        "kind":1,
        "startTimeUnixNano":"1700000000000000000",
        "endTimeUnixNano":"1700000000000000000",
        "attributes":[{"key":"gently.event","value":{"stringValue":"SessionStart"}}],
        "status":{"code":0}
      }]
    }]
  }]
}
```

Times and 64-bit `intValue` values are decimal JSON strings. IDs are lowercase
hex strings: 32 characters for `traceId`, 16 for `spanId` and `parentSpanId`.
Kind and status codes are JSON numbers, not strings. Gently's current resource
and span attribute values use `stringValue`, including counts and byte lengths.

## Resource attributes (per session)

| Key | Meaning |
| --- | --- |
| `service.name` | `gently` |
| `gently.harness` | `claude-code` or `codex` |
| `gently.session_id` | Harness session identifier |
| `gently.tenant_id`, `gently.device_id` | Capture namespace; Worker verifies or injects these from authentication |
| `gently.cwd` | Working directory from the hook |
| `host.name` | Local hostname, or `unknown` |
| `os.type` | Rust platform OS value, such as `macos` or `linux` |
| `gently.version` | Crate version |
| `gently.tmux_pane` | Optional `TMUX_PANE` value |
| `gently.transcript_path` | Optional harness-provided transcript path |

Resource attributes accompany every event envelope. Optional context keys are
omitted when empty. Paths, hostnames, pane IDs, and session IDs identify the local
environment even when prompt and tool content are absent or encrypted.

## Span fields

| Field | Wire shape |
| --- | --- |
| `traceId`, `spanId` | Hex strings |
| `parentSpanId` | Hex string; omitted for a span without a parent |
| `name` | String, such as `session`, `turn:1`, or a tool name |
| `kind` | Numeric OTLP code; internal `1`, client/tool `3` |
| `startTimeUnixNano`, `endTimeUnixNano` | Decimal strings, emitted for every Gently span |
| `attributes` | Array of `{key, value}` objects |
| `status` | Object with numeric `code`: `0` unset, `1` OK, `2` error; optional `message` |

A provisional span commonly has equal start and end timestamps and unset status.
A later report uses the same span ID with updated end, status, and attributes.
The adapters' [hook mappings](hooks.md) determine which lifecycle events create
or update each logical span.

## Span attributes (`gently.*`)

Attributes vary with harness and event. Common metadata includes `gently.event`,
`gently.tool_name`, `gently.tool_use_id`, `gently.permission_mode`,
`gently.agent_type`, `gently.agent_transcript_path`, and optional model, effort,
or session-source values. They are not all present on every span.

Selected content has byte lengths and, only after approved encrypted capture,
opaque references:

```json
[
  {"key":"gently.prompt.raw_ref","value":{"stringValue":"0123456789abcdef0123456789abcdef"}},
  {"key":"gently.prompt.bytes","value":{"stringValue":"42"}}
]
```

`.bytes` is UTF-8 or compact-JSON byte length encoded as a string. No public
`.sha256` content fingerprints are emitted, including with capture disabled.
A random event reference points to an immutable encrypted field map; field
names and owning span IDs are authenticated inside the ciphertext. The
ciphertext is retained separately from metadata, and optional sync uses the
raw endpoint rather than OTLP attributes.

Reader resolution can add decrypted fields to in-memory CLI/MCP results after
verifying tenant/session/device and field-to-span bindings. It never adds them
to exported OTLP. See [security and privacy](../concepts/security-and-privacy.md)
and [raw enrollment](../guides/encrypted-raw-values.md).

## Collector query shape

The Worker flattens export fields into snake-case span rows. OTLP camel-case
names therefore do not match the query names directly: `spanId` becomes
`span_id`, for example. `attrs_json` and `resource_json` are JSON-encoded strings,
not the original enclosing export request. The Worker discards instrumentation
scope metadata and `status.message`, and lifts session, harness, and tool context
into query columns. Other OTLP fields outside the flattened row shape are not
preserved as separate fields.

Only the single-trace query adds effective display bounds. These are computed
from stored rows and are not OTLP wire fields. See the
[Worker reference](worker.md#query-response-fields) for field lists and bounds.
`gently waterfall` accepts query-row arrays, not export envelopes.

## Deliberate deviations

Gently uses deterministic identifiers derived from harness session IDs and
logical span keys. Repeated reports of the same logical span reuse those IDs.
This lets separate hook processes reconstruct parent links without relying on
random IDs allocated by another process.

Session, turn, and subagent spans can be emitted provisionally and later updated,
instead of only emitting a finished span once. The included Worker merges these
reports by span ID, retaining earliest start, latest end-or-start, and content
from the latest-ending report. Conflicting content at equal ending timestamps
favors the incoming report. See [ingest behavior](worker.md#ingest-and-repeated-span-updates).

A different backend must handle these repeated reports as intended; merely
accepting OTLP/JSON does not establish that it will merge them. Duplicate or
provisional records can otherwise remain. CLI and MCP queries also require the
Worker's custom query API, so changing the export destination alone does not
make those commands compatible with another backend.

## Source

[Wire types and serialization](https://github.com/JamieAP/gently/blob/main/crates/gently-core/src/otlp.rs),
[ID derivation](https://github.com/JamieAP/gently/blob/main/crates/gently-core/src/ids.rs), and
[content lengths](https://github.com/JamieAP/gently/blob/main/crates/gently-harness/src/hooks.rs) define the emitted
format.
