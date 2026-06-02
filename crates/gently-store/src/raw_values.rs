//! Local-only SHA-to-raw string lookup table.
//!
//! These rows are never exported. They let a local CLI/MCP reader resolve the
//! digest attributes that were already sent to the collector back into raw
//! prompt/tool strings when the caller explicitly opts in.

use crate::{Result, Store};

impl Store {
    /// Store one raw value by the same SHA string emitted in span attributes.
    pub fn raw_value_put(&self, sha256: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO raw_values (sha256, value, created_unix_nano)
             VALUES (?1, ?2, strftime('%s','now') * 1000000000)
             ON CONFLICT(sha256) DO UPDATE SET value = excluded.value",
            rusqlite::params![sha256, value],
        )?;
        Ok(())
    }

    /// Resolve one raw value by SHA.
    pub fn raw_value_get(&self, sha256: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM raw_values WHERE sha256 = ?1")?;
        let mut rows = stmt.query(rusqlite::params![sha256])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Number of locally captured raw values.
    pub fn raw_values_len(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM raw_values", [], |r| r.get(0))?;
        Ok(n as usize)
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
    fn put_and_get_raw_value() {
        let (_d, s) = store();
        s.raw_value_put("abc123", "secret text").unwrap();
        assert_eq!(
            s.raw_value_get("abc123").unwrap().as_deref(),
            Some("secret text")
        );
        assert_eq!(s.raw_value_get("missing").unwrap(), None);
        assert_eq!(s.raw_values_len().unwrap(), 1);
    }

    #[test]
    fn put_same_sha_replaces_value() {
        let (_d, s) = store();
        s.raw_value_put("abc123", "old").unwrap();
        s.raw_value_put("abc123", "new").unwrap();
        assert_eq!(s.raw_values_len().unwrap(), 1);
        assert_eq!(s.raw_value_get("abc123").unwrap().as_deref(), Some("new"));
    }
}
