import schemaSql from "../schema.sql?raw";

// Migrations by file name, which orders them as Wrangler applies them.
const migrations = Object.entries(import.meta.glob<string>("../migrations/*.sql", {
  query: "?raw", import: "default", eager: true,
})).sort(([a], [b]) => a.localeCompare(b));

// Apply SQL one statement at a time. Statements end with ";" at the end of a
// line; a CREATE TRIGGER statement ends only at its "END;".
async function applySql(db: D1Database, sql: string, name: string): Promise<void> {
  let statement = "";
  for (const line of sql.split("\n")) {
    if (line.trim().startsWith("--")) continue;
    statement += `${line}\n`;
    const text = statement.trim();
    if (text.endsWith(";") && (!/^CREATE TRIGGER/i.test(text) || /\bEND;$/i.test(text))) {
      await db.prepare(text).run();
      statement = "";
    }
  }
  if (statement.trim()) throw new Error(`${name} ends inside a statement`);
}

export async function applySchema(db: D1Database): Promise<void> {
  await applySql(db, schemaSql, "schema.sql");
}

// Each migration runs once, after schema.sql, as `wrangler d1 migrations apply`
// does for a deployed database.
export async function applyMigrations(db: D1Database): Promise<void> {
  if (migrations.length === 0) throw new Error("no migrations found");
  for (const [name, sql] of migrations) await applySql(db, sql, name);
}
