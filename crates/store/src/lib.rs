//! The event store: everything the OS saw and did, in SQLite (WAL mode).
use rusqlite::{params, Connection};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: i64,
    pub at_ms: u64,
    pub kind: String,
    pub payload: String,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }
    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                at_ms INTEGER NOT NULL,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS events_kind_time ON events(kind, at_ms);
             -- events are facts: they are never edited or deleted
             CREATE TRIGGER IF NOT EXISTS events_no_update BEFORE UPDATE ON events
               BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
             CREATE TRIGGER IF NOT EXISTS events_no_delete BEFORE DELETE ON events
               BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;",
        )?;
        Ok(Self { conn })
    }

    pub fn append(&self, at_ms: u64, kind: &str, payload: &str) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO events (at_ms, kind, payload) VALUES (?1, ?2, ?3)",
            params![at_ms as i64, kind, payload],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn by_kind(&self, kind: &str) -> rusqlite::Result<Vec<Event>> {
        let mut st = self
            .conn
            .prepare("SELECT id, at_ms, kind, payload FROM events WHERE kind = ?1 ORDER BY id")?;
        let rows = st.query_map([kind], |r| {
            Ok(Event {
                id: r.get(0)?,
                at_ms: r.get::<_, i64>(1)? as u64,
                kind: r.get(2)?,
                payload: r.get(3)?,
            })
        })?;
        rows.collect()
    }

    pub fn count(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
    }

    pub fn journal_mode(&self) -> rusqlite::Result<String> {
        self.conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_read_back() {
        let s = Store::in_memory().unwrap();
        s.append(10, "route", "plan -> commercial").unwrap();
        s.append(20, "route", "chat -> local-small").unwrap();
        s.append(30, "gate", "minted").unwrap();
        let r = s.by_kind("route").unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].payload, "chat -> local-small");
        assert_eq!(s.count().unwrap(), 3);
    }
    #[test]
    fn events_cannot_be_edited() {
        let s = Store::in_memory().unwrap();
        let id = s.append(1, "x", "y").unwrap();
        let e = s
            .conn
            .execute("UPDATE events SET payload='z' WHERE id=?1", [id])
            .unwrap_err();
        assert!(e.to_string().contains("append-only"));
    }
    #[test]
    fn events_cannot_be_deleted() {
        let s = Store::in_memory().unwrap();
        s.append(1, "x", "y").unwrap();
        assert!(s.conn.execute("DELETE FROM events", []).is_err());
        assert_eq!(s.count().unwrap(), 1);
    }
    #[test]
    fn file_store_uses_wal_and_survives_reopen() {
        let p = std::env::temp_dir().join(format!("store-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&p);
        {
            let s = Store::open(&p).unwrap();
            assert_eq!(s.journal_mode().unwrap(), "wal");
            s.append(5, "k", "persisted").unwrap();
        }
        let s = Store::open(&p).unwrap();
        assert_eq!(s.by_kind("k").unwrap()[0].payload, "persisted");
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", p.display(), ext));
        }
    }
}
