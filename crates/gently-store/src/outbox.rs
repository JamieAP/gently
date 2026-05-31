//! The export outbox: completed OTLP span JSON awaiting delivery.

use crate::{Result, Store};

impl Store {
    /// Append one completed span's OTLP/JSON to the outbox.
    pub fn outbox_enqueue(&self, span_json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO outbox (span_json, created_unix_nano, attempts)
             VALUES (?1, strftime('%s','now') * 1000000000, 0)",
            rusqlite::params![span_json],
        )?;
        Ok(())
    }

    /// Number of rows currently buffered.
    pub fn outbox_len(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// Take up to `n` oldest rows as `(id, span_json)` for a send batch.
    pub fn outbox_take_batch(&self, n: usize) -> Result<Vec<(i64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, span_json FROM outbox ORDER BY id ASC LIMIT ?1")?;
        let rows = stmt
            .query_map(rusqlite::params![n as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Delete rows by id after a successful send.
    pub fn outbox_delete(&self, ids: &[i64]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for id in ids {
            tx.execute("DELETE FROM outbox WHERE id = ?1", rusqlite::params![id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Increment the attempt counter for rows after a failed send.
    pub fn outbox_bump_attempts(&self, ids: &[i64]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for id in ids {
            tx.execute(
                "UPDATE outbox SET attempts = attempts + 1 WHERE id = ?1",
                rusqlite::params![id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Drop oldest rows beyond `cap`, returning how many were dropped.
    pub fn outbox_trim(&self, cap: usize) -> Result<usize> {
        let dropped = self.conn.execute(
            "DELETE FROM outbox WHERE id IN (
               SELECT id FROM outbox ORDER BY id DESC LIMIT -1 OFFSET ?1
             )",
            rusqlite::params![cap as i64],
        )?;
        Ok(dropped)
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
    fn enqueue_take_delete_roundtrip() {
        let (_d, s) = store();
        s.outbox_enqueue("{\"a\":1}").unwrap();
        s.outbox_enqueue("{\"a\":2}").unwrap();
        assert_eq!(s.outbox_len().unwrap(), 2);
        let batch = s.outbox_take_batch(10).unwrap();
        assert_eq!(batch.len(), 2);
        let ids: Vec<i64> = batch.iter().map(|(id, _)| *id).collect();
        s.outbox_delete(&ids).unwrap();
        assert_eq!(s.outbox_len().unwrap(), 0);
    }

    #[test]
    fn bump_attempts_keeps_rows() {
        let (_d, s) = store();
        s.outbox_enqueue("{}").unwrap();
        let ids: Vec<i64> = s
            .outbox_take_batch(10)
            .unwrap()
            .iter()
            .map(|(id, _)| *id)
            .collect();
        s.outbox_bump_attempts(&ids).unwrap();
        assert_eq!(s.outbox_len().unwrap(), 1);
    }

    #[test]
    fn trim_drops_oldest_beyond_cap() {
        let (_d, s) = store();
        for i in 0..10 {
            s.outbox_enqueue(&format!("{{\"n\":{i}}}")).unwrap();
        }
        let dropped = s.outbox_trim(4).unwrap();
        assert_eq!(dropped, 6);
        assert_eq!(s.outbox_len().unwrap(), 4);
        // the 4 NEWEST survive
        let remaining = s.outbox_take_batch(10).unwrap();
        assert!(remaining[0].1.contains("\"n\":6"));
    }
}
