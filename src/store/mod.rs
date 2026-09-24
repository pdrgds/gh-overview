mod commands;
mod prs;
mod state;

use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use rusqlite::{Connection, TransactionBehavior};

pub use commands::Command;
pub use prs::{PrRow, Tab};

const MIGRATIONS: &[&str] = &[include_str!("schema_v1.sql")];

pub struct Store {
    conn: Connection,
}

pub struct Db<'a> {
    conn: &'a Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            bail!("could not enable WAL journal mode (got {mode})");
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        migrate(&mut conn)?;
        Ok(Store { conn })
    }

    pub fn db(&self) -> Db<'_> {
        Db { conn: &self.conn }
    }

    pub fn tx<T>(&mut self, f: impl FnOnce(&Db<'_>) -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&Db { conn: &tx })?;
        tx.commit()?;
        Ok(out)
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version as usize > i {
            continue;
        }
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_idempotent_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        Store::open(&path).unwrap().db().set_meta("k", "v").unwrap();
        let store = Store::open(&path).unwrap();
        assert_eq!(store.db().meta("k").unwrap().as_deref(), Some("v"));
    }

    #[test]
    fn a_failed_migration_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE meta (x TEXT);")
            .unwrap();
        assert!(Store::open(&path).is_err());
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
        let prs: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master WHERE name = 'prs'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!((version, prs), (0, 0));
    }

    #[test]
    fn transactions_take_the_write_lock_up_front() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let mut store = Store::open(&path).unwrap();
        let other = Connection::open(&path).unwrap();
        other.busy_timeout(Duration::ZERO).unwrap();
        store
            .tx(|db| {
                db.meta("k")?;
                assert!(
                    other
                        .execute("INSERT INTO commands (kind, created_at) VALUES ('refresh', 'now')", [])
                        .is_err()
                );
                db.set_meta("k", "v")
            })
            .unwrap();
        assert_eq!(store.db().meta("k").unwrap().as_deref(), Some("v"));
    }

    #[test]
    fn file_stores_use_wal_with_normal_sync() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        let sync: i64 = store
            .conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();
        assert_eq!(sync, 1);
    }

    #[test]
    fn failed_transaction_rolls_back() {
        let mut store = Store::open_in_memory().unwrap();
        let result: Result<()> = store.tx(|db| {
            db.set_meta("k", "v")?;
            anyhow::bail!("boom")
        });
        assert!(result.is_err());
        assert_eq!(store.db().meta("k").unwrap(), None);
    }
}
