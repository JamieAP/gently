import schemaSql from "../schema.sql?raw";

// Apply schema.sql one statement at a time. Statements end with ";" at the end
// of a line; a CREATE TRIGGER statement ends only at its "END;".
export async function applySchema(db: D1Database): Promise<void> {
  let statement = "";
  for (const line of schemaSql.split("\n")) {
    if (line.trim().startsWith("--")) continue;
    statement += `${line}\n`;
    const text = statement.trim();
    if (text.endsWith(";") && (!/^CREATE TRIGGER/i.test(text) || /\bEND;$/i.test(text))) {
      await db.prepare(text).run();
      statement = "";
    }
  }
  if (statement.trim()) throw new Error("schema.sql ends inside a statement");
}
