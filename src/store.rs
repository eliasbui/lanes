//! The list of downloads in SQLite, so the app finds them again after a restart. The live progress lives in the
//! engine and its sidecars; this keeps the lasting facts: where each file goes, its size, and whether it finished.

use crate::engine::state;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// One row of the download list, as the page shows it before the live progress is laid over it.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub id: i64,
    pub url: String,
    pub filename: String,
    pub dir: String,
    pub size: Option<i64>,
    pub status: u8,
    pub added_at: i64,
    pub error: String,
    /// Referer, Cookie and User-Agent from the browser, stored as a JSON array of pairs.
    pub headers: String,
}

impl Row {
    pub fn final_path(&self) -> PathBuf {
        Path::new(&self.dir).join(&self.filename)
    }

    /// The stored headers back as pairs for the engine; an empty list when there are none.
    pub fn header_pairs(&self) -> Vec<(String, String)> {
        serde_json::from_str(&self.headers).unwrap_or_default()
    }
}

/// Opens the list, making the table the first time.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS downloads (
            id        INTEGER PRIMARY KEY,
            url       TEXT NOT NULL,
            filename  TEXT NOT NULL,
            dir       TEXT NOT NULL,
            size      INTEGER,
            status    INTEGER NOT NULL,
            added_at  INTEGER NOT NULL,
            error     TEXT NOT NULL DEFAULT '',
            headers   TEXT NOT NULL DEFAULT '[]'
        );",
    )?;
    Ok(db)
}

/// Records a new download and gives its id. `headers` is a JSON array of pairs, `[]` when there are none.
pub fn add(db: &Connection, url: &str, filename: &str, dir: &str, headers: &str, now: i64) -> rusqlite::Result<i64> {
    db.execute(
        "INSERT INTO downloads (url, filename, dir, size, status, added_at, headers) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)",
        params![url, filename, dir, state::QUEUED, now, headers],
    )?;
    Ok(db.last_insert_rowid())
}

/// Saves the size once it is known.
pub fn set_size(db: &Connection, id: i64, size: u64) -> rusqlite::Result<()> {
    db.execute("UPDATE downloads SET size = ?1 WHERE id = ?2", params![size as i64, id])?;
    Ok(())
}

/// Saves a new state, and the reason when it failed.
pub fn set_status(db: &Connection, id: i64, status: u8, error: &str) -> rusqlite::Result<()> {
    db.execute("UPDATE downloads SET status = ?1, error = ?2 WHERE id = ?3", params![status, error, id])?;
    Ok(())
}

pub fn remove(db: &Connection, id: i64) -> rusqlite::Result<()> {
    db.execute("DELETE FROM downloads WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn get(db: &Connection, id: i64) -> rusqlite::Result<Option<Row>> {
    db.query_row("SELECT id, url, filename, dir, size, status, added_at, error, headers FROM downloads WHERE id = ?1", params![id], row)
        .optional()
}

/// Every download, newest first.
pub fn list(db: &Connection) -> rusqlite::Result<Vec<Row>> {
    let mut stmt =
        db.prepare("SELECT id, url, filename, dir, size, status, added_at, error, headers FROM downloads ORDER BY added_at DESC, id DESC")?;
    let rows = stmt.query_map([], row)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        url: r.get(1)?,
        filename: r.get(2)?,
        dir: r.get(3)?,
        size: r.get(4)?,
        status: r.get(5)?,
        added_at: r.get(6)?,
        error: r.get(7)?,
        headers: r.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_is_added_listed_and_removed() {
        let db = open(Path::new(":memory:")).unwrap();
        let id = add(&db, "https://x.com/a.zip", "a.zip", "/tmp", "[]", 100).unwrap();
        set_size(&db, id, 4096).unwrap();
        set_status(&db, id, state::DONE, "").unwrap();
        let rows = list(&db).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].filename, "a.zip");
        assert_eq!(rows[0].size, Some(4096));
        assert_eq!(rows[0].status, state::DONE);
        assert_eq!(rows[0].final_path(), PathBuf::from("/tmp/a.zip"));
        assert_eq!(get(&db, id).unwrap().unwrap().url, "https://x.com/a.zip");
        remove(&db, id).unwrap();
        assert!(list(&db).unwrap().is_empty());
    }

    #[test]
    fn a_failure_keeps_its_reason() {
        let db = open(Path::new(":memory:")).unwrap();
        let id = add(&db, "https://x.com/a", "a", "/tmp", "[]", 1).unwrap();
        set_status(&db, id, state::ERROR, "Máy chủ trả mã 404").unwrap();
        assert_eq!(get(&db, id).unwrap().unwrap().error, "Máy chủ trả mã 404");
    }
}
