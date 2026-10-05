CREATE TABLE IF NOT EXISTS spans (
  tenant_id TEXT NOT NULL,
  span_id TEXT NOT NULL,
  source_device_id TEXT NOT NULL,
  trace_id TEXT NOT NULL,
  parent_span_id TEXT,
  name TEXT NOT NULL,
  kind INTEGER NOT NULL,
  start_unix_nano TEXT NOT NULL,
  end_unix_nano TEXT,
  status INTEGER NOT NULL DEFAULT 0,
  session_id TEXT,
  harness TEXT,
  tool_name TEXT,
  tool_use_id TEXT,
  attrs_json TEXT,
  resource_json TEXT,
  ingested_unix_nano TEXT NOT NULL,
  PRIMARY KEY (tenant_id, span_id)
);
CREATE INDEX IF NOT EXISTS idx_spans_trace ON spans(tenant_id, trace_id);
CREATE INDEX IF NOT EXISTS idx_spans_session ON spans(tenant_id, session_id);
CREATE INDEX IF NOT EXISTS idx_spans_start ON spans(tenant_id, start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_tool ON spans(tenant_id, tool_name);
CREATE INDEX IF NOT EXISTS idx_spans_trace_start ON spans(tenant_id, trace_id, start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_session_start ON spans(tenant_id, session_id, start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_harness_start ON spans(tenant_id, harness, start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_tool_start ON spans(tenant_id, tool_name, start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_name_start ON spans(tenant_id, name, start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_status_start ON spans(tenant_id, status, start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_kind_start ON spans(tenant_id, kind, start_unix_nano);

CREATE TABLE IF NOT EXISTS raw_values (
  tenant_id TEXT NOT NULL,
  raw_ref TEXT NOT NULL,
  device_id TEXT NOT NULL,
  key_epoch INTEGER NOT NULL CHECK (key_epoch > 0),
  envelope_json TEXT NOT NULL,
  created_unix_nano TEXT NOT NULL,
  PRIMARY KEY (tenant_id, raw_ref)
);
