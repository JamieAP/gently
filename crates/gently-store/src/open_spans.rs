//! Pending-span tracking and the per-session turn counter.

use crate::{Result, Store};
use rusqlite::OptionalExtension;

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

    /// Inspect an open span without consuming its lifecycle state.
    pub fn peek_open(&self, session_id: &str, logical_key: &str) -> Result<Option<OpenSpan>> {
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
            .optional()?;
        Ok(row)
    }

    /// Find a referenced open span in a root and its exact child-scope prefix.
    /// Ambiguous references return None rather than choosing an execution agent.
    /// A literal prefix comparison avoids interpreting harness IDs as SQL patterns.
    pub fn unique_open_span_id_in_scopes(
        &self,
        root_session_id: &str,
        child_scope_prefix: &str,
        logical_key: &str,
    ) -> Result<Option<String>> {
        let mut statement = self.conn.prepare(
            "SELECT span_id FROM open_spans
             WHERE logical_key = ?1 AND
               (session_id = ?2 OR substr(session_id, 1, length(?3)) = ?3)
             LIMIT 2",
        )?;
        let mut rows = statement.query(rusqlite::params![
            logical_key,
            root_session_id,
            child_scope_prefix
        ])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let span_id = row.get(0)?;
        if rows.next()?.is_some() {
            return Ok(None);
        }
        Ok(Some(span_id))
    }

    /// Remove and return the open span for `(session, logical_key)`, if any.
    pub fn take_open(&self, session_id: &str, logical_key: &str) -> Result<Option<OpenSpan>> {
        let row = self.peek_open(session_id, logical_key)?;
        if row.is_some() {
            self.conn.execute(
                "DELETE FROM open_spans WHERE session_id = ?1 AND logical_key = ?2",
                rusqlite::params![session_id, logical_key],
            )?;
        }
        Ok(row)
    }

    /// Reopen a session, turn or agent while retaining its earliest start and
    /// original parent. SQLite performs the minimum atomically across hooks.
    pub fn reopen_provisional(&self, s: &OpenSpan) -> Result<OpenSpan> {
        self.conn.execute(
            "INSERT INTO open_spans
             (session_id, logical_key, span_id, parent_span_id, name, kind, start_unix_nano, attrs_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(session_id, logical_key) DO UPDATE SET
               start_unix_nano = MIN(open_spans.start_unix_nano, excluded.start_unix_nano),
               parent_span_id = COALESCE(open_spans.parent_span_id, excluded.parent_span_id),
               attrs_json = excluded.attrs_json",
            rusqlite::params![s.session_id, s.logical_key, s.span_id, s.parent_span_id,
                s.name, s.kind, s.start_unix_nano as i64, s.attrs_json],
        )?;
        self.peek_open(&s.session_id, &s.logical_key)?
            .ok_or_else(|| rusqlite::Error::QueryReturnedNoRows.into())
    }

    /// Drop provisional open spans whose start is older than `cutoff_unix_nano`,
    /// returning how many were removed. A span's closing event may never arrive
    /// during crashes, older harness releases, or disabled hooks, so rows would otherwise
    /// accumulate without bound. Provisional spans are queued for export on
    /// open; delivery is not assured. This only bounds local bookkeeping growth
    /// without inventing an end. A later close may no longer recover the original
    /// start. Sessions interleave on one host, so a later event is no signal that
    /// an earlier span has closed.
    pub fn reap_open_spans(&self, cutoff_unix_nano: u64) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM open_spans WHERE start_unix_nano < ?1",
            rusqlite::params![cutoff_unix_nano as i64],
        )?;
        Ok(n)
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

    /// Stable per-session ordinal (1-based) for a harness-provided turn id, plus
    /// whether this call is the **first sight** of that `turn_id`. Assigns the
    /// next ordinal on first sight; idempotent thereafter. Lets a harness that
    /// supplies its own turn ids (Codex's `turn_id`) key turn spans by that
    /// stable id while keeping clean `turn:1`/`turn:2` display names. Assignment
    /// is atomic under SQLite's write lock, so concurrent hook processes never
    /// collide. The first-sight flag lets the applier lazily emit a turn span the
    /// first time a turn is referenced by ANY event - Codex starts turns (tasks)
    /// that fire no `UserPromptSubmit` hook, so a tool can be the first to name a
    /// turn, and its parent turn span must exist or the tool dangles.
    pub fn turn_ordinal(&self, session_id: &str, turn_id: &str) -> Result<(u64, bool)> {
        let inserted = self.conn.execute(
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
        Ok((n as u64, inserted > 0))
    }

    /// Observe the current counter turn, plus whether this is the session's
    /// first counter reference. Turn zero groups activity before an observed
    /// prompt without inventing a prompt boundary. The retained counter row
    /// prevents later references from reopening an already closed turn.
    pub fn observe_current_turn(&self, session_id: &str) -> Result<(u64, bool)> {
        let inserted = self.conn.execute(
            "INSERT INTO counters (session_id, turn_index, current_turn) VALUES (?1, 0, 0)
             ON CONFLICT(session_id) DO NOTHING",
            rusqlite::params![session_id],
        )?;
        Ok((self.current_turn(session_id)?, inserted > 0))
    }

    /// The current turn index for a session (0 if no turn has started).
    pub fn current_turn(&self, session_id: &str) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT current_turn FROM counters WHERE session_id = ?1",
                rusqlite::params![session_id],
                |r| r.get(0),
            )
            .optional()?
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
    fn reap_open_spans_drops_only_rows_older_than_cutoff() {
        let (_d, s) = store();
        let mk = |key: &str, start: u64| super::OpenSpan {
            session_id: "s".into(),
            logical_key: key.into(),
            span_id: key.into(),
            parent_span_id: None,
            name: key.into(),
            kind: 1,
            start_unix_nano: start,
            attrs_json: "[]".into(),
        };
        s.open_span(&mk("old", 100)).unwrap();
        s.open_span(&mk("fresh", 1_000)).unwrap();

        // Cutoff between the two: only the stale row is reaped.
        assert_eq!(s.reap_open_spans(500).unwrap(), 1);
        assert_eq!(s.take_open("s", "old").unwrap(), None, "stale row gone");
        assert!(s.take_open("s", "fresh").unwrap().is_some(), "fresh kept");
        // Idempotent: nothing left older than cutoff.
        s.open_span(&mk("fresh", 1_000)).unwrap();
        assert_eq!(s.reap_open_spans(500).unwrap(), 0);
    }

    #[test]
    fn current_turn_defaults_to_zero() {
        let (_d, s) = store();
        assert_eq!(s.current_turn("nope").unwrap(), 0);
    }

    #[test]
    fn observing_current_turn_infers_only_the_initial_unprompted_reference() {
        let (_d, s) = store();
        assert_eq!(s.observe_current_turn("sess").unwrap(), (0, true));
        assert_eq!(s.observe_current_turn("sess").unwrap(), (0, false));
        assert_eq!(s.next_turn_index("sess").unwrap(), 1);
        assert_eq!(s.observe_current_turn("sess").unwrap(), (1, false));
        assert_eq!(s.next_turn_index("other").unwrap(), 1);
        assert_eq!(s.observe_current_turn("other").unwrap(), (1, false));
    }

    #[test]
    fn current_turn_propagates_database_errors() {
        let (_d, s) = store();
        s.conn.execute("DROP TABLE counters", []).unwrap();
        assert!(
            s.current_turn("sess").is_err(),
            "a broken counter must not silently become turn zero"
        );
    }

    #[test]
    fn turn_ordinal_assigns_stable_per_session_ordinals() {
        let (_d, s) = store();
        assert_eq!(s.turn_ordinal("sess", "tid-a").unwrap(), (1, true));
        assert_eq!(s.turn_ordinal("sess", "tid-b").unwrap(), (2, true));
        assert_eq!(
            s.turn_ordinal("sess", "tid-a").unwrap(),
            (1, false),
            "idempotent for same turn_id; not first sight"
        );
        assert_eq!(s.turn_ordinal("sess", "tid-c").unwrap(), (3, true));
        assert_eq!(
            s.turn_ordinal("other", "tid-a").unwrap(),
            (1, true),
            "per-session"
        );
    }
}
