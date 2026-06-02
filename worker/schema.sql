CREATE TABLE IF NOT EXISTS spans (
  span_id TEXT PRIMARY KEY,
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
  ingested_unix_nano TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_spans_trace ON spans(trace_id);
CREATE INDEX IF NOT EXISTS idx_spans_session ON spans(session_id);
CREATE INDEX IF NOT EXISTS idx_spans_start ON spans(start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_tool ON spans(tool_name);
CREATE INDEX IF NOT EXISTS idx_spans_trace_start ON spans(trace_id, start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_session_start ON spans(session_id, start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_harness_start ON spans(harness, start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_tool_start ON spans(tool_name, start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_name_start ON spans(name, start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_status_start ON spans(status, start_unix_nano);
CREATE INDEX IF NOT EXISTS idx_spans_kind_start ON spans(kind, start_unix_nano);
