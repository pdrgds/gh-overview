use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use crate::config::Config;
use crate::store::Db;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Ok { polled_ago: Option<String> },
    DaemonDown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderStatus {
    pub health: Health,
    pub errors: Vec<String>,
    pub degraded: Option<String>,
    pub muted_until: Option<DateTime<Utc>>,
}

impl Default for HeaderStatus {
    fn default() -> Self {
        HeaderStatus {
            health: Health::DaemonDown,
            errors: vec![],
            degraded: None,
            muted_until: None,
        }
    }
}

pub fn ago(d: Duration) -> String {
    let s = d.num_seconds().max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=172_799 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

fn parse_time(value: Option<String>) -> Option<DateTime<Utc>> {
    value
        .and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
        .map(|t| t.with_timezone(&Utc))
}

pub fn header_status(db: &Db<'_>, config: &Config, now: DateTime<Utc>) -> Result<HeaderStatus> {
    let heartbeat = parse_time(db.meta("heartbeat")?);
    let alive = heartbeat.is_some_and(|hb| now - hb <= (config.poll() * 3).max(Duration::minutes(2)));
    let mut errors = Vec::new();
    let mut last_polls = Vec::new();
    for account in &config.accounts {
        let last_poll = parse_time(db.meta(&format!("last_poll:{}", account.login))?);
        if let Some(t) = last_poll {
            last_polls.push(t);
        }
        if let Some(err) = db.meta(&format!("last_error:{}", account.login))? {
            let since = last_poll.map_or("never ok".to_string(), |t| format!("last ok {} ago", ago(now - t)));
            errors.push(format!("{}: {err} ({since})", account.label));
        }
    }
    let health = if alive {
        Health::Ok {
            polled_ago: last_polls.into_iter().max().map(|t| ago(now - t)),
        }
    } else {
        Health::DaemonDown
    };
    Ok(HeaderStatus {
        health,
        errors,
        degraded: db.meta("notifier_degraded")?,
        muted_until: parse_time(db.meta("muted_until")?).filter(|until| *until > now),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::testkit::t;
    use crate::store::Store;

    fn config() -> Config {
        let mut c = Config::with_accounts(vec!["me-work".into(), "me-home".into()]);
        c.accounts[1].label = "personal".into();
        c
    }

    #[test]
    fn ago_formats() {
        assert_eq!(ago(Duration::seconds(12)), "12s");
        assert_eq!(ago(Duration::minutes(5)), "5m");
        assert_eq!(ago(Duration::hours(3)), "3h");
        assert_eq!(ago(Duration::days(3)), "3d");
    }

    #[test]
    fn daemon_down_without_fresh_heartbeat() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(
            header_status(&store.db(), &config(), t(0)).unwrap().health,
            Health::DaemonDown
        );
        store.db().set_meta("heartbeat", &t(0).to_rfc3339()).unwrap();
        assert_eq!(
            header_status(&store.db(), &config(), t(4)).unwrap().health,
            Health::DaemonDown
        );
    }

    #[test]
    fn healthy_with_errors_per_account() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        db.set_meta("heartbeat", &t(10).to_rfc3339()).unwrap();
        db.set_meta("last_poll:me-work", &t(10).to_rfc3339()).unwrap();
        db.set_meta("last_poll:me-home", &t(0).to_rfc3339()).unwrap();
        db.set_meta("last_error:me-home", "gh auth token failed").unwrap();
        let status = header_status(&db, &config(), t(10)).unwrap();
        assert_eq!(
            status.health,
            Health::Ok {
                polled_ago: Some("0s".into())
            }
        );
        assert_eq!(status.errors, vec!["personal: gh auth token failed (last ok 10m ago)"]);
    }

    #[test]
    fn errors_before_any_successful_poll_and_a_degraded_notifier() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        db.set_meta("heartbeat", &t(0).to_rfc3339()).unwrap();
        db.set_meta("last_error:me-home", "gh auth token failed").unwrap();
        db.set_meta("notifier_degraded", "notifications are off for gh-overview")
            .unwrap();
        let status = header_status(&db, &config(), t(0)).unwrap();
        assert_eq!(status.health, Health::Ok { polled_ago: None });
        assert_eq!(status.errors, vec!["personal: gh auth token failed (never ok)"]);
        assert_eq!(
            status.degraded.as_deref(),
            Some("notifications are off for gh-overview")
        );
    }

    #[test]
    fn only_a_future_mute_shows() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        assert_eq!(header_status(&db, &config(), t(0)).unwrap().muted_until, None);
        db.set_meta("muted_until", &t(60).to_rfc3339()).unwrap();
        assert_eq!(header_status(&db, &config(), t(0)).unwrap().muted_until, Some(t(60)));
        assert_eq!(header_status(&db, &config(), t(60)).unwrap().muted_until, None);
    }
}
