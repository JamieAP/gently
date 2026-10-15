-- Spans reference their resource (schema.sql: span_resources) instead of
-- repeating it. SQLite cannot add a column idempotently, so this runs once.
-- Existing rows move to references; any written later by an earlier Worker
-- keep their inline copy, which reads the same.
ALTER TABLE spans ADD COLUMN resource_id INTEGER;
INSERT OR IGNORE INTO span_resources (tenant_id, device_id, resource_json)
  SELECT DISTINCT tenant_id, source_device_id, resource_json FROM spans
  WHERE resource_json IS NOT NULL;
UPDATE spans SET
  resource_id = (SELECT r.id FROM span_resources r
    WHERE r.tenant_id = spans.tenant_id AND r.device_id = spans.source_device_id
      AND r.resource_json = spans.resource_json),
  resource_json = NULL
WHERE resource_json IS NOT NULL;
