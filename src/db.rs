use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

/// Schema migrations, applied in order. `PRAGMA user_version` stores how many
/// have run. Never edit a shipped migration: append a new file instead.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_playlists.sql"),
    include_str!("../migrations/0003_jobs.sql"),
    include_str!("../migrations/0004_job_problems.sql"),
];

/// Opens the database, sets connection pragmas and applies pending migrations.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let mut db = Connection::open(path)?;
    // WAL lets the UI read while a scanner writes on another connection.
    db.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
    db.pragma_update(None, "synchronous", "NORMAL")?;
    db.pragma_update(None, "foreign_keys", true)?;
    db.busy_timeout(Duration::from_secs(5))?;
    migrate(&mut db)?;
    Ok(db)
}

fn migrate(db: &mut Connection) -> rusqlite::Result<()> {
    let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = db.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_db() -> Connection {
        let mut db = Connection::open_in_memory().unwrap();
        db.pragma_update(None, "foreign_keys", true).unwrap();
        migrate(&mut db).unwrap();
        db
    }

    #[test]
    fn migrations_run_once() {
        let mut db = memory_db();
        migrate(&mut db).unwrap();
        let version: u32 = db
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version as usize, MIGRATIONS.len());
        let tabs: u32 = db
            .query_row("SELECT count(*) FROM category", [], |row| row.get(0))
            .unwrap();
        assert_eq!(tabs, 9, "eight from 0001, Videos from 0002");
    }

    #[test]
    fn open_file_uses_wal_and_reopens() {
        let path = std::env::temp_dir().join(format!("elsewhere-test-{}.db", std::process::id()));
        for _ in 0..2 {
            let db = open(&path).unwrap();
            let mode: String = db
                .pragma_query_value(None, "journal_mode", |row| row.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
        }
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }

    #[test]
    fn rescan_upsert_keeps_item_id() {
        let db = memory_db();
        db.execute_batch(
            "INSERT INTO library (category_id, root) VALUES (7, '/manga');
             INSERT INTO item (category_id, library_id, kind, path, title, sort_title, favorite)
             VALUES (7, 1, 'group', 'Frieren', 'Frieren', 'frieren', 1);",
        )
        .unwrap();
        let upsert = "INSERT INTO item (category_id, library_id, kind, path, title, sort_title)
                      VALUES (7, 1, 'group', 'Frieren', 'Frieren (renamed)', 'frieren')
                      ON CONFLICT (library_id, path) DO UPDATE
                      SET title = excluded.title, missing_since = NULL
                      RETURNING id, favorite";
        let (id, favorite): (i64, i64) = db
            .query_row(upsert, [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!((id, favorite), (1, 1));
    }

    #[test]
    fn foreign_keys_enforced() {
        let db = memory_db();
        let orphan = db.execute(
            "INSERT INTO item (category_id, kind, title, sort_title) VALUES (999, 'entry', 'x', 'x')",
            [],
        );
        assert!(orphan.is_err());
    }
}
