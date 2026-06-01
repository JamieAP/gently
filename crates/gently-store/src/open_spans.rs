//! Pending-span tracking and the per-session turn counter.

use crate::{Result, Store};

/// A span recorded at its opening event, awaiting its closing event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenSpan {
    pub session_id: String,
    pub logical_key: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: u8,
    pub start_unix_nano: u64,
    pub attrs_json: String,
}

impl Store {
    /// Record an opening span. Re-opening the same `(session, logical_key)`
    /// overwrites - the latest start wins (e.g. a retried hook).
    pub fn open_span(&self, s: &OpenSpan) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO open_spans
             (session_id, logical_key, span_id, parent_span_id, name, kind, start_unix_nano, attrs_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                s.session_id,
                s.logical_key,
                s.span_id,
                s.parent_span_id,
                s.name,
                s.kind,
                s.start_unix_nano as i64,
                s.attrs_json,
            ],
        )?;
        Ok(())
    }

    /// Remove and return the open span for `(session, logical_key)`, if any.
    pub fn take_open(&self, session_id: &str, logical_key: &str) -> Result<Option<OpenSpan>> {
        let row = self
            .conn
            .query_row(
                "SELECT span_id, parent_span_id, name, kind, start_unix_nano, attrs_json
                 FROM open_spans WHERE session_id = ?1 AND logical_key = ?2",
                rusqlite::params![session_id, logical_key],
                |r| {
                    Ok(OpenSpan {
                        session_id: session_id.to_string(),
                        logical_key: logical_key.to_string(),
                        span_id: r.get(0)?,
                        parent_span_id: r.get(1)?,
                        name: r.get(2)?,
                        kind: r.get(3)?,
                        start_unix_nano: r.get::<_, i64>(4)? as u64,
                        attrs_json: r.get(5)?,
                    })
                },
            )
            .ok();
        if row.is_some() {
            self.conn.execute(
                "DELETE FROM open_spans WHERE session_id = ?1 AND logical_key = ?2",
                rusqlite::params![session_id, logical_key],
            )?;
        }
        Ok(row)
    }

    /// Increment and return the next turn index for a session, and mark it as
    /// the current turn (used to parent tool spans). Starts at 1.
    pub fn next_turn_index(&self, session_id: &str) -> Result<u64> {
        self.conn.execute(
            "INSERT INTO counters (session_id, turn_index, current_turn) VALUES (?1, 1, 1)
             ON CONFLICT(session_id) DO UPDATE SET
               turn_index = turn_index + 1,
               current_turn = turn_index + 1",
            rusqlite::params![session_id],
        )?;
        let n: i64 = self.conn.query_row(
            "SELECT turn_index FROM counters WHERE session_id = ?1",
            rusqlite::params![session_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Stable per-session ordinal (1-based) for a harness-provided turn id.
    /// Assigns the next ordinal on first sight of a `turn_id`; idempotent
    /// thereafter. Lets a harness that supplies its own turn ids (Codex's
    /// `turn_id`) key turn spans by that stable id while keeping clean
    /// `turn:1`/`turn:2` display names. Assignment is atomic under SQLite's
    /// write lock, so concurrent hook processes never collide.
    pub fn turn_ordinal(&self, session_id: &str, turn_id: &str) -> Result<u64> {
        self.conn.execute(
            "INSERT INTO turn_ordinals (session_id, turn_id, ordinal)
             VALUES (?1, ?2,
               (SELECT COALESCE(MAX(ordinal), 0) + 1 FROM turn_ordinals WHERE session_id = ?1))
             ON CONFLICT(session_id, turn_id) DO NOTHING",
            rusqlite::params![session_id, turn_id],
        )?;
        let n: i64 = self.conn.query_row(
            "SELECT ordinal FROM turn_ordinals WHERE session_id = ?1 AND turn_id = ?2",
            rusqlite::params![session_id, turn_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// The current turn index for a session (0 if no turn has started - tool
    /// calls before the first prompt parent to `turn:0`, a degraded but valid
    /// state).
    pub fn current_turn(&self, session_id: &str) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT current_turn FROM counters WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(n as u64)
    }
}

#[cfg(test)]
mod tests {
    use crate::Store;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(&dir.path().join("state.db")).unwrap();
        (dir, s)
    }

    #[test]
    fn turn_index_is_monotonic() {
        let (_d, s) = store();
        assert_eq!(s.next_turn_index("sess").unwrap(), 1);
        assert_eq!(s.next_turn_index("sess").unwrap(), 2);
        assert_eq!(s.next_turn_index("sess").unwrap(), 3);
        assert_eq!(s.current_turn("sess").unwrap(), 3);
        assert_eq!(s.next_turn_index("other").unwrap(), 1);
    }

    #[test]
    fn take_open_removes_the_row() {
        let (_d, s) = store();
        let span = super::OpenSpan {
            session_id: "s".into(),
            logical_key: "tool:tu_1".into(),
            span_id: "abcd".into(),
            parent_span_id: Some("ef01".into()),
            name: "Bash".into(),
            kind: 3,
            start_unix_nano: 42,
            attrs_json: "{}".into(),
        };
        s.open_span(&span).unwrap();
        assert_eq!(s.take_open("s", "tool:tu_1").unwrap().as_ref(), Some(&span));
        assert_eq!(s.take_open("s", "tool:tu_1").unwrap(), None);
    }

    #[test]
    fn current_turn_defaults_to_zero() {
        let (_d, s) = store();
        assert_eq!(s.current_turn("nope").unwrap(), 0);
    }

    #[test]
    fn turn_ordinal_assigns_stable_per_session_ordinals() {
        let (_d, s) = store();
        assert_eq!(s.turn_ordinal("sess", "tid-a").unwrap(), 1);
        assert_eq!(s.turn_ordinal("sess", "tid-b").unwrap(), 2);
        assert_eq!(
            s.turn_ordinal("sess", "tid-a").unwrap(),
            1,
            "idempotent for same turn_id"
        );
        assert_eq!(s.turn_ordinal("sess", "tid-c").unwrap(), 3);
        assert_eq!(s.turn_ordinal("other", "tid-a").unwrap(), 1, "per-session");
    }
}
