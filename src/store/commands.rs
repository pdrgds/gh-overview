use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;
use tracing::warn;

use super::Db;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Ack { pr_key: String },
    Snooze { pr_key: String, until: DateTime<Utc> },
    Done { pr_key: String },
    Refresh,
}

impl Db<'_> {
    pub fn enqueue(&self, command: &Command, now: DateTime<Utc>) -> Result<()> {
        let (kind, pr_key, until) = match command {
            Command::Ack { pr_key } => ("ack", Some(pr_key.as_str()), None),
            Command::Snooze { pr_key, until } => ("snooze", Some(pr_key.as_str()), Some(*until)),
            Command::Done { pr_key } => ("done", Some(pr_key.as_str()), None),
            Command::Refresh => ("refresh", None, None),
        };
        self.conn.execute(
            "INSERT INTO commands (kind, pr_key, until, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![kind, pr_key, until, now],
        )?;
        Ok(())
    }

    pub fn has_commands(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT EXISTS(SELECT 1 FROM commands)", [], |row| row.get(0))?)
    }

    pub fn take_commands(&self) -> Result<Vec<Command>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, kind, pr_key, until FROM commands ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let decoded = (|| -> rusqlite::Result<(String, Option<String>, Option<DateTime<Utc>>)> {
                Ok((row.get(1)?, row.get(2)?, row.get(3)?))
            })();
            Ok((id, decoded))
        })?;
        let mut commands = Vec::new();
        let mut max_id = 0;
        for row in rows {
            let (id, decoded) = row?;
            max_id = id;
            let Ok((kind, pr_key, until)) = decoded else {
                warn!("dropping undecodable command {id} from the queue");
                continue;
            };
            let command = match (kind.as_str(), pr_key, until) {
                ("ack", Some(pr_key), _) => Command::Ack { pr_key },
                ("snooze", Some(pr_key), Some(until)) => Command::Snooze { pr_key, until },
                ("done", Some(pr_key), _) => Command::Done { pr_key },
                ("refresh", _, _) => Command::Refresh,
                (other, _, _) => {
                    warn!("dropping malformed command {other:?} from the queue");
                    continue;
                }
            };
            commands.push(command);
        }
        self.conn
            .execute("DELETE FROM commands WHERE id <= ?1", params![max_id])?;
        Ok(commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::testkit::t;
    use crate::store::Store;

    #[test]
    fn commands_are_taken_in_order_exactly_once() {
        let mut store = Store::open_in_memory().unwrap();
        let sent = vec![
            Command::Ack { pr_key: "a#1".into() },
            Command::Snooze {
                pr_key: "a#2".into(),
                until: t(15),
            },
            Command::Done { pr_key: "a#3".into() },
            Command::Refresh,
        ];
        assert!(!store.db().has_commands().unwrap());
        for c in &sent {
            store.db().enqueue(c, t(0)).unwrap();
        }
        assert!(store.db().has_commands().unwrap());
        assert_eq!(store.tx(|db| db.take_commands()).unwrap(), sent);
        assert!(!store.db().has_commands().unwrap());
        assert!(store.tx(|db| db.take_commands()).unwrap().is_empty());
    }

    #[test]
    fn malformed_rows_are_dropped_without_blocking_the_queue() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .db()
            .conn
            .execute_batch(
                "INSERT INTO commands (kind, pr_key, created_at) VALUES ('snooze', 'a#1', 'x');
                 INSERT INTO commands (kind, created_at) VALUES ('from-a-newer-tui', 'x');",
            )
            .unwrap();
        store.db().enqueue(&Command::Refresh, t(0)).unwrap();
        assert_eq!(store.tx(|db| db.take_commands()).unwrap(), vec![Command::Refresh]);
        assert!(store.tx(|db| db.take_commands()).unwrap().is_empty());
    }

    #[test]
    fn undecodable_rows_are_dropped_too() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .db()
            .conn
            .execute(
                "INSERT INTO commands (kind, pr_key, until, created_at) VALUES ('snooze', 'a#1', 'not-a-date', 'x')",
                [],
            )
            .unwrap();
        store.db().enqueue(&Command::Refresh, t(0)).unwrap();
        assert_eq!(store.tx(|db| db.take_commands()).unwrap(), vec![Command::Refresh]);
        assert!(store.tx(|db| db.take_commands()).unwrap().is_empty());
    }
}
