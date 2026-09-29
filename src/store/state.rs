use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};
use tracing::warn;

use super::Db;
use crate::domain::alert::AlertState;

impl Db<'_> {
    pub fn is_seen(&self, activity_id: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM activity WHERE id = ?1", params![activity_id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    pub fn has_activity_for(&self, pr_key: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM activity WHERE pr_key = ?1 AND substr(id, 1, 3) != 'rr:' LIMIT 1",
                params![pr_key],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn has_marker(&self, pr_key: &str, prefix: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM activity WHERE pr_key = ?1 AND substr(id, 1, length(?2)) = ?2 LIMIT 1",
                params![pr_key, prefix],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn mark_seen(&self, pr_key: &str, activity_id: &str, now: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO activity (id, pr_key, seen_at) VALUES (?1, ?2, ?3)",
            params![activity_id, pr_key, now],
        )?;
        Ok(())
    }

    pub fn alert(&self, pr_key: &str) -> Result<AlertState> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT state FROM alerts WHERE pr_key = ?1", params![pr_key], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(match json.map(|json| serde_json::from_str(&json)) {
            Some(Ok(state)) => state,
            Some(Err(err)) => {
                warn!("resetting unreadable alert for {pr_key}: {err}");
                AlertState::default()
            }
            None => AlertState::default(),
        })
    }

    pub fn put_alert(&self, pr_key: &str, state: &AlertState) -> Result<()> {
        if *state == AlertState::default() {
            self.conn
                .execute("DELETE FROM alerts WHERE pr_key = ?1", params![pr_key])?;
        } else {
            self.conn.execute(
                "INSERT INTO alerts (pr_key, state) VALUES (?1, ?2)
                 ON CONFLICT(pr_key) DO UPDATE SET state = excluded.state",
                params![pr_key, serde_json::to_string(state)?],
            )?;
        }
        Ok(())
    }

    pub fn alerts(&self) -> Result<Vec<(String, AlertState)>> {
        let mut stmt = self.conn.prepare("SELECT pr_key, state FROM alerts ORDER BY pr_key")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        let mut alerts = Vec::new();
        for row in rows {
            let (key, json) = row?;
            match serde_json::from_str(&json) {
                Ok(state) => alerts.push((key, state)),
                Err(err) => warn!("skipping unreadable alert for {key}: {err}"),
            }
        }
        Ok(alerts)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |row| row.get(0))
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn retain_account_meta(&self, accounts: &[&str]) -> Result<usize> {
        let mut stmt = self.conn.prepare(
            "SELECT key FROM meta WHERE key LIKE 'bootstrapped:%' OR key LIKE 'review_events_bootstrapped:%' OR key LIKE 'last_poll:%' OR key LIKE 'last_error:%'",
        )?;
        let keys: Vec<String> = stmt.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
        let mut removed = 0;
        for key in keys {
            let account = key.split_once(':').map_or("", |(_, account)| account);
            if !accounts.contains(&account) {
                removed += self.conn.execute("DELETE FROM meta WHERE key = ?1", params![key])?;
            }
        }
        Ok(removed)
    }

    pub fn delete_meta(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM meta WHERE key = ?1", params![key])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::alert::AlertState;
    use crate::domain::testkit::t;
    use crate::store::Store;

    #[test]
    fn seen_set() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        assert!(!db.is_seen("PRR_1").unwrap());
        db.mark_seen("acme/api#1", "PRR_1", t(0)).unwrap();
        db.mark_seen("acme/api#1", "PRR_1", t(1)).unwrap();
        assert!(db.is_seen("PRR_1").unwrap());
    }

    #[test]
    fn alert_defaults_round_trips_and_deletes_when_default() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        assert_eq!(db.alert("k").unwrap(), AlertState::default());
        let state = AlertState {
            generation: 3,
            done_at: Some(t(1)),
            ..Default::default()
        };
        db.put_alert("k", &state).unwrap();
        assert_eq!(db.alert("k").unwrap(), state);
        assert_eq!(db.alerts().unwrap(), vec![("k".to_string(), state)]);
        db.put_alert("k", &AlertState::default()).unwrap();
        assert!(db.alerts().unwrap().is_empty());
    }

    #[test]
    fn meta_upsert_and_delete() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        db.set_meta("heartbeat", "a").unwrap();
        db.set_meta("heartbeat", "b").unwrap();
        assert_eq!(db.meta("heartbeat").unwrap().as_deref(), Some("b"));
        db.delete_meta("heartbeat").unwrap();
        assert_eq!(db.meta("heartbeat").unwrap(), None);
    }

    #[test]
    fn activity_is_tracked_per_pr() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        assert!(!db.has_activity_for("acme/api#1").unwrap());
        db.mark_seen("acme/api#1", "PRR_1", t(0)).unwrap();
        assert!(db.has_activity_for("acme/api#1").unwrap());
        assert!(!db.has_activity_for("acme/api#2").unwrap());
    }

    #[test]
    fn markers_are_found_by_prefix_per_pr() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        db.mark_seen("acme/a#1", "rr:work:RRE_1", t(0)).unwrap();
        assert!(db.has_marker("acme/a#1", "rr:work:").unwrap());
        assert!(!db.has_marker("acme/a#1", "rr:home:").unwrap());
        assert!(!db.has_marker("acme/b#2", "rr:work:").unwrap());
    }

    #[test]
    fn review_request_markers_do_not_make_a_pr_known() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        db.mark_seen("acme/a#1", "rr:work:RRE_1", t(0)).unwrap();
        assert!(!db.has_activity_for("acme/a#1").unwrap());
        db.mark_seen("acme/a#1", "PRR_1", t(0)).unwrap();
        assert!(db.has_activity_for("acme/a#1").unwrap());
    }

    #[test]
    fn account_meta_is_pruned_for_removed_accounts() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        for key in [
            "bootstrapped:work",
            "last_poll:work",
            "bootstrapped:gone",
            "review_events_bootstrapped:gone",
            "last_error:gone",
            "heartbeat",
        ] {
            db.set_meta(key, "x").unwrap();
        }
        assert_eq!(db.retain_account_meta(&["work"]).unwrap(), 3);
        assert!(db.meta("bootstrapped:work").unwrap().is_some());
        assert!(db.meta("bootstrapped:gone").unwrap().is_none());
        assert!(db.meta("heartbeat").unwrap().is_some());
    }

    #[test]
    fn unreadable_alerts_are_skipped_and_reset() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        db.put_alert(
            "good",
            &AlertState {
                generation: 1,
                ..Default::default()
            },
        )
        .unwrap();
        db.conn
            .execute("INSERT INTO alerts (pr_key, state) VALUES ('bad', 'not json')", [])
            .unwrap();
        let keys: Vec<String> = db.alerts().unwrap().into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec!["good"]);
        assert_eq!(db.alert("bad").unwrap(), AlertState::default());
    }
}
