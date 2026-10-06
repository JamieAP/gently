import { ClientError } from "./http.js";
import type { Principal } from "./auth.js";

// OTLP/JSON types and span flattening.
// uint64 fields (traceId, spanId, parentSpanId, *UnixNano) are kept as strings
// throughout - never Number() them (JS precision loss on uint64).

export interface Row {
  span_id: string;
  trace_id: string;
  parent_span_id: string | null;
  name: string;
  kind: number;
  start_unix_nano: string;
  end_unix_nano: string | null;
  status: number;
  session_id: string | null;
  harness: string | null;
  tool_name: string | null;
  tool_use_id: string | null;
  attrs_json: string;
  resource_json: string;
  ingested_unix_nano: string;
}

interface OtlpKeyValue {
  key: string;
  value: { stringValue?: string; intValue?: string | number };
}

interface OtlpStatus {
  code?: number;
}

interface OtlpSpan {
  traceId: string;
  spanId: string;
  parentSpanId?: string;
  name: string;
  kind?: number;
  startTimeUnixNano: string;
  endTimeUnixNano?: string;
  attributes?: OtlpKeyValue[];
  status?: OtlpStatus;
}

interface OtlpScopeSpans {
  spans?: OtlpSpan[];
}

interface OtlpResource {
  attributes?: OtlpKeyValue[];
}

interface OtlpResourceSpans {
  resource?: OtlpResource;
  scopeSpans?: OtlpScopeSpans[];
}

interface OtlpRequest {
  resourceSpans?: OtlpResourceSpans[];
}

function attrString(attrs: OtlpKeyValue[] | undefined, key: string): string | null {
  if (!attrs) return null;
  for (const kv of attrs) {
    if (kv.key === key) {
      const sv = kv.value.stringValue;
      return sv !== undefined ? sv : null;
    }
  }
  return null;
}


const RAW_FIELDS = new Set([
  "prompt", "user", "user_prompt", "assistant", "assistant_message", "last_assistant_message",
  "tool_input", "tool_response", "input", "output", "tool_output", "tool_result", "response", "raw",
  "compact_instructions", "compact_summary", "custom_instructions", "tool_calls", "error", "error_details", "hook_payload", "message.delta", "instruction_file",
]);

const SAFE_REASONS = new Set(["clear", "logout", "prompt_input_exit", "bypass_permissions_disabled", "other", "exit", "shutdown"]);
// At most 32 upserts plus ownership reads fit the Free plan's 50-query budget.
const MAX_INGEST_SPANS = 32;

function forbiddenRawKey(key: string): boolean {
  if (key.endsWith(".sha256")) return true;
  const unprefixed = key.startsWith("gently.") ? key.slice("gently.".length) : key;
  return RAW_FIELDS.has(unprefixed);
}

/** Guard against accidentally uploading the raw aliases captured by Gently.
 * This cannot classify arbitrary text: exported metadata remains visible to the collector.
 */
function validateMetadata(req: OtlpRequest): void {
  if (!req || typeof req !== "object" || Array.isArray(req) ||
      Object.keys(req).some(key => key !== "resourceSpans") ||
      (req.resourceSpans !== undefined && !Array.isArray(req.resourceSpans))) {
    throw new ClientError(400, "Invalid metadata envelope");
  }
  const visit = (value: unknown, depth: number): void => {
    if (depth > 64) throw new ClientError(400, "Invalid metadata envelope");
    if (Array.isArray(value)) {
      for (const child of value) visit(child, depth + 1);
    } else if (value && typeof value === "object") {
      const attribute = value as { key?: unknown; value?: { stringValue?: unknown } };
      if (attribute.key === "gently.reason" || attribute.key === "reason") {
        const reason = attribute.value?.stringValue;
        if (typeof reason !== "string" || !SAFE_REASONS.has(reason)) {
          throw new ClientError(400, "Raw content cannot be uploaded as metadata");
        }
      }
      for (const [key, child] of Object.entries(value)) {
        if ((key === "reason" || key === "gently.reason") && (typeof child !== "string" || !SAFE_REASONS.has(child))) {
          throw new ClientError(400, "Raw content cannot be uploaded as metadata");
        }
        if (forbiddenRawKey(key) || (key === "key" && typeof child === "string" && forbiddenRawKey(child))) {
          throw new ClientError(400, "Raw content cannot be uploaded as metadata");
        }
        visit(child, depth + 1);
      }
    }
  };
  visit(req, 0);
}

function verifyHostClaims(attrs: OtlpKeyValue[], principal: Principal): void {
  for (const [key, expected] of [["gently.tenant_id", principal.tenant_id], ["gently.device_id", principal.device_id]]) {
    const claims = attrs.filter(attribute => attribute.key === key);
    if (claims.length > 1 || (claims.length === 1 && claims[0].value?.stringValue !== expected)) {
      throw new ClientError(403, "Capture host claim differs from authenticated device");
    }
  }
}

export function flatten(req: OtlpRequest, principal: Principal): Row[] {
  validateMetadata(req);
  const rows: Row[] = [];
  const ingested_unix_nano = String(Date.now() * 1_000_000);

  for (const rs of req.resourceSpans ?? []) {
    const resourceAttrs = rs.resource?.attributes ?? [];
    verifyHostClaims(resourceAttrs, principal);
    const trustedResourceAttrs = [...resourceAttrs];
    for (const [key, value] of [["gently.tenant_id", principal.tenant_id], ["gently.device_id", principal.device_id]]) {
      if (!resourceAttrs.some(attribute => attribute.key === key)) trustedResourceAttrs.push({ key, value: { stringValue: value } });
    }
    const resource_json = JSON.stringify(trustedResourceAttrs);

    // session_id and harness are trace-scoped: they live on the OTLP resource,
    // not per span. Lift them once and apply to every span in this group.
    const resSessionId = attrString(resourceAttrs, "gently.session_id");
    const resHarness = attrString(resourceAttrs, "gently.harness");

    for (const ss of rs.scopeSpans ?? []) {
      for (const span of ss.spans ?? []) {
        if (rows.length >= MAX_INGEST_SPANS) {
          throw new ClientError(413, "Too many spans in metadata envelope");
        }
        const spanAttrs = span.attributes ?? [];
        verifyHostClaims(spanAttrs, principal);

        // Trace-scoped attrs come from the resource (with a span-level override
        // for robustness); tool_name/tool_use_id are genuinely per-span.
        const session_id = attrString(spanAttrs, "gently.session_id") ?? resSessionId;
        const harness = attrString(spanAttrs, "gently.harness") ?? resHarness;
        const tool_name = attrString(spanAttrs, "gently.tool_name");
        const tool_use_id = attrString(spanAttrs, "gently.tool_use_id");

        // Remaining attributes (excluding lifted ones)
        const gentlyKeys = new Set([
          "gently.session_id",
          "gently.harness",
          "gently.tool_name",
          "gently.tool_use_id",
        ]);
        const remainingAttrs = spanAttrs.filter((kv) => !gentlyKeys.has(kv.key));
        const attrs_json = JSON.stringify(remainingAttrs);

        rows.push({
          span_id: span.spanId,
          trace_id: span.traceId,
          parent_span_id: span.parentSpanId ?? null,
          name: span.name,
          kind: span.kind ?? 0,
          // Keep as string - never Number()
          start_unix_nano: span.startTimeUnixNano,
          end_unix_nano: span.endTimeUnixNano ?? null,
          status: span.status?.code ?? 0,
          session_id,
          harness,
          tool_name,
          tool_use_id,
          attrs_json,
          resource_json,
          ingested_unix_nano,
        });
      }
    }
  }

  return rows;
}
