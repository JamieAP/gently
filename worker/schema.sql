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

-- Paged trace reads (worker/src/d1.ts). Rows are read in position order,
-- (length(start), start, span_id) folded into one key, so a page is an index
-- seek rather than a sort of the whole trace. The key expressions must stay
-- structurally identical to `position` and `END_POSITION` in d1.ts.
CREATE INDEX IF NOT EXISTS idx_spans_trace_position ON spans(tenant_id, trace_id,
  (printf('%09d', length(start_unix_nano)) || ':' || start_unix_nano || ':' || span_id));
CREATE INDEX IF NOT EXISTS idx_spans_trace_end ON spans(tenant_id, trace_id,
  (printf('%09d', length(COALESCE(end_unix_nano, start_unix_nano))) || ':' || COALESCE(end_unix_nano, start_unix_nano)));
CREATE INDEX IF NOT EXISTS idx_spans_trace_parent ON spans(tenant_id, trace_id, parent_span_id);

-- A trace's generation advances on any write that could hide a row from a paged
-- read already in progress: an insert before the trace's last position, a
-- changed start time, or a delete. A paged read that sees the generation change
-- gets HTTP 409. Appends at the end of a trace (the usual live-capture write)
-- and merges that keep the start leave it unchanged, so reading an active trace
-- is not restarted needlessly. A trace with no row here is at generation 0.
CREATE TABLE IF NOT EXISTS trace_generations (
  tenant_id TEXT NOT NULL,
  trace_id TEXT NOT NULL,
  generation INTEGER NOT NULL,
  PRIMARY KEY (tenant_id, trace_id)
);
CREATE TRIGGER IF NOT EXISTS trace_generation_on_insert AFTER INSERT ON spans
WHEN EXISTS (
  SELECT 1 FROM spans later
  WHERE later.tenant_id = NEW.tenant_id AND later.trace_id = NEW.trace_id
    AND (printf('%09d', length(later.start_unix_nano)) || ':' || later.start_unix_nano || ':' || later.span_id)
      > (printf('%09d', length(NEW.start_unix_nano)) || ':' || NEW.start_unix_nano || ':' || NEW.span_id)
)
BEGIN
  INSERT INTO trace_generations (tenant_id, trace_id, generation) VALUES (NEW.tenant_id, NEW.trace_id, 1)
    ON CONFLICT (tenant_id, trace_id) DO UPDATE SET generation = generation + 1;
END;
CREATE TRIGGER IF NOT EXISTS trace_generation_on_start AFTER UPDATE OF start_unix_nano ON spans
WHEN OLD.start_unix_nano IS NOT NEW.start_unix_nano
BEGIN
  INSERT INTO trace_generations (tenant_id, trace_id, generation) VALUES (NEW.tenant_id, NEW.trace_id, 1)
    ON CONFLICT (tenant_id, trace_id) DO UPDATE SET generation = generation + 1;
END;
CREATE TRIGGER IF NOT EXISTS trace_generation_on_delete AFTER DELETE ON spans
BEGIN
  INSERT INTO trace_generations (tenant_id, trace_id, generation) VALUES (OLD.tenant_id, OLD.trace_id, 1)
    ON CONFLICT (tenant_id, trace_id) DO UPDATE SET generation = generation + 1;
END;
