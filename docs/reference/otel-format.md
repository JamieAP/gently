# OTel format

gently emits OTLP/JSON. The envelope is standard; two patterns are deliberately
non-idiomatic (see [Deliberate deviations](#deliberate-deviations)).

## Envelope

Standard OTLP `ExportTraceServiceRequest`:

```
resourceSpans[] → { resource{attributes[]}, scopeSpans[] → { scope, spans[] } }
```

`scope` is `{name:"gently", version:…}`. All 64-bit integers and ids are JSON
**strings** (uint64-precision-safe): `traceId` 32-hex, `spanId`/`parentSpanId`
16-hex, `*TimeUnixNano` decimal strings.

## Resource attributes (per session)

| key | example |
|---|---|
| `service.name` | `gently` |
| `gently.harness` | `claude-code` |
| `gently.session_id` | `SESSION_ID` |
| `gently.cwd` | `/path/to/project` |
| `host.name` | `host.local` |
| `os.type` | `macos` |
| `gently.version` | `0.1.0` |

## Span fields

`traceId`, `spanId`, `parentSpanId`, `name`, `kind` (1 Internal / 3 Client),
`startTimeUnixNano`, `endTimeUnixNano`, `attributes[]`, `status{code}` (0 unset /
1 ok / 2 error).

## Span attributes (`gently.*`)

`event`, `tool_name`, `tool_use_id`, `permission_mode`, and digest pairs
`…sha256` + `…bytes` for tool input/response and prompts. Keys are unique
(close-event values win on merge). No raw content - see
[Security & privacy](../concepts/security-and-privacy.md).

## Deliberate deviations

The bytes are valid OTLP/JSON, but **don't point the exporter at a generic
backend (Tempo/Jaeger/Honeycomb) without accounting for these** - they work only
because gently owns its collector:

1. **Deterministic ids** where OTel recommends random - the price of stateless
   reconstruction and idempotency.
2. **Provisional-then-final double emit** of the same `span_id`, where standard
   OTel emits each span once at end. gently relies on the collector doing
   last-write-wins by `span_id`; a backend that doesn't upsert would show
   duplicates. The payoff is crash-durable in-flight spans, which the stable OTel
   model doesn't offer.

To target a standard backend: emit on close only (losing crash durability), or
ensure the backend dedups by `span_id`.
