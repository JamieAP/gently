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

export function flatten(req: OtlpRequest): Row[] {
  const rows: Row[] = [];
  const ingested_unix_nano = String(Date.now() * 1_000_000);

  for (const rs of req.resourceSpans ?? []) {
    const resourceAttrs = rs.resource?.attributes ?? [];
    const resource_json = JSON.stringify(resourceAttrs);

    // session_id and harness are trace-scoped: they live on the OTLP resource,
    // not per span. Lift them once and apply to every span in this group.
    const resSessionId = attrString(resourceAttrs, "gently.session_id");
    const resHarness = attrString(resourceAttrs, "gently.harness");

    for (const ss of rs.scopeSpans ?? []) {
      for (const span of ss.spans ?? []) {
        const spanAttrs = span.attributes ?? [];

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
