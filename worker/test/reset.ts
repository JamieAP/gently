import schemaSql from "../schema.sql?raw";

// Vitest 4 isolates storage per test file, not per test. Drop every table that
// exists rather than a hand-maintained list, so a table added to schema.sql (or
// created by a test) cannot carry rows into the next test, then reapply the
// schema. SQLite and D1 internal tables are left alone.
export async function resetDatabase(db: D1Database): Promise<void> {
  const { results } = await db.prepare(
    "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' AND name NOT LIKE '\\_cf\\_%' ESCAPE '\\'",
  ).all<{ name: string }>();
  for (const { name } of results) {
    await db.prepare(`DROP TABLE IF EXISTS "${name.replaceAll('"', '""')}"`).run();
  }
  // Statements end at a semicolon followed by a line break, so a future trigger
  // body's internal semicolons stay inside its CREATE TRIGGER statement.
  for (const statement of schemaSql.split(/;\s*(?:\r?\n|$)/).map(s => s.trim()).filter(Boolean)) {
    await db.prepare(statement).run();
  }
}
