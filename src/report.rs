use std::fmt::Write;

use anyhow::Result;
use chrono::{DateTime, Local, Utc};

use crate::config::Config;
use crate::domain::activity::{activity_of, summarize};
use crate::domain::actors::Identity;
use crate::domain::alert::Phase;
use crate::domain::model::AccountSnapshot;
use crate::domain::reasons::{describe, reasons};
use crate::store::Db;
use crate::tui::status::{Health, ago, header_status};

pub fn status_report(db: &Db<'_>, config: &Config, now: DateTime<Utc>) -> Result<String> {
    let mut out = String::new();
    let status = header_status(db, config, now)?;
    match status.health {
        Health::DaemonDown => writeln!(out, "daemon: not running (run `ghov install`)")?,
        Health::Ok { .. } => writeln!(out, "daemon: running")?,
    }
    for account in &config.accounts {
        let last_poll = db
            .meta(&format!("last_poll:{}", account.login))?
            .and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
            .map_or("never".to_string(), |t| {
                format!("{} ago", ago(now - t.with_timezone(&Utc)))
            });
        let error = db
            .meta(&format!("last_error:{}", account.login))?
            .unwrap_or_else(|| "ok".into());
        writeln!(
            out,
            "{} ({}): last poll {last_poll}, {error}",
            account.label, account.login
        )?;
    }
    if let Some(degraded) = status.degraded {
        writeln!(out, "notifier: {degraded}")?;
    }
    for (key, state) in db.alerts()? {
        let phase = match state.phase(now) {
            Phase::Idle => continue,
            Phase::Pinging => "pinging".to_string(),
            Phase::Acked => match state.remind_at {
                Some(at) => format!("seen, reminds {}", at.with_timezone(&Local).format("%a %H:%M")),
                None => "seen".to_string(),
            },
            Phase::Snoozed => format!(
                "snoozed until {}",
                state
                    .snoozed_until
                    .expect("snoozed has until")
                    .with_timezone(&Local)
                    .format("%a %H:%M")
            ),
        };
        let summary = summarize(&state.pending);
        if summary.is_empty() {
            writeln!(out, "{key}: {phase}")?;
        } else {
            writeln!(out, "{key}: {phase} — {summary}")?;
        }
    }
    Ok(out)
}

pub fn snapshot_report(snapshot: &AccountSnapshot, identity: &Identity) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "account {} (rate limit remaining: {:?})",
        snapshot.login, snapshot.rate_remaining
    );
    for pr in &snapshot.mine {
        let why = describe(&reasons(pr, identity));
        let activity = activity_of(pr, identity);
        let _ = writeln!(
            out,
            "mine   {} {:?} [{}] activity: {}",
            pr.base.key(),
            pr.base.title,
            if why.is_empty() { "tackled" } else { &why },
            if activity.is_empty() {
                "none".to_string()
            } else {
                summarize(&activity)
            },
        );
    }
    for request in &snapshot.to_review {
        let via = match (&request.team, request.direct) {
            (Some(team), true) => format!("direct, team:{team}"),
            (Some(team), false) => format!("team:{team}"),
            (None, _) => "direct".to_string(),
        };
        let _ = writeln!(
            out,
            "review {} {:?} by {} ({via})",
            request.base.key(),
            request.base.title,
            request.base.author
        );
    }
    for line in snapshot.errors.iter().chain(&snapshot.truncated) {
        let _ = writeln!(out, "warning: {line}");
    }
    if snapshot.withheld_mine {
        let _ = writeln!(
            out,
            "warning: some authored PRs were withheld (e.g. SSO), the daemon keeps their rows for up to 1h"
        );
    }
    if snapshot.withheld_review {
        let _ = writeln!(
            out,
            "warning: some review requests were withheld (e.g. SSO), the daemon keeps their rows for up to 1h"
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::alert::AlertState;
    use crate::domain::model::{Author, ReviewRequest, ReviewState};
    use crate::domain::testkit::*;
    use crate::store::Store;

    #[test]
    fn status_lists_accounts_and_active_alerts() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        let config = Config::with_accounts(vec!["me-work".into()]);
        db.set_meta("heartbeat", &t(0).to_rfc3339()).unwrap();
        db.set_meta("last_poll:me-work", &t(-2).to_rfc3339()).unwrap();
        let pinging = AlertState {
            cycle_started_at: Some(t(0)),
            generation: 1,
            ..Default::default()
        };
        db.put_alert("acme/api#1", &pinging).unwrap();
        db.put_alert(
            "acme/api#2",
            &AlertState {
                done_at: Some(t(0)),
                ..Default::default()
            },
        )
        .unwrap();
        let text = status_report(&db, &config, t(0)).unwrap();
        assert_eq!(
            text,
            "daemon: running\nme-work (me-work): last poll 2m ago, ok\nacme/api#1: pinging\n"
        );
    }

    #[test]
    fn snapshot_report_describes_prs() {
        let mut pr = my_pr("acme/api", 1);
        pr.reviews = vec![review(
            "r1",
            Author::user("alice"),
            ReviewState::ChangesRequested,
            0,
            "head",
        )];
        let snapshot = AccountSnapshot {
            login: "me-work".into(),
            mine: vec![pr],
            to_review: vec![ReviewRequest {
                base: base("acme/web", 2),
                direct: false,
                team: Some("fe".into()),
                event: None,
                reviews: vec![],
            }],
            truncated: vec!["acme/web#9: more than 50 reviews, older ones ignored".into()],
            ..Default::default()
        };
        assert_eq!(
            snapshot_report(&snapshot, &identity()),
            "account me-work (rate limit remaining: None)\n\
             mine   acme/api#1 \"PR 1\" [changes: alice] activity: alice requested changes\n\
             review acme/web#2 \"PR 2\" by me-work (team:fe)\n\
             warning: acme/web#9: more than 50 reviews, older ones ignored\n"
        );
    }

    #[test]
    fn status_shows_seen_alerts_summaries_and_a_degraded_notifier() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        let config = Config::with_accounts(vec!["me-work".into()]);
        db.set_meta("heartbeat", &t(0).to_rfc3339()).unwrap();
        db.set_meta("notifier_degraded", "notifications are off for gh-overview")
            .unwrap();
        db.set_meta("last_error:me-work", "http error: timeout").unwrap();
        let mut pr = my_pr("acme/api", 1);
        pr.reviews = vec![review(
            "r1",
            Author::user("alice"),
            ReviewState::ChangesRequested,
            0,
            "head",
        )];
        let seen = AlertState {
            cycle_started_at: Some(t(0)),
            acked_at: Some(t(1)),
            pending: crate::domain::activity::activity_of(&pr, &identity()),
            generation: 1,
            ..Default::default()
        };
        db.put_alert("acme/api#1", &seen).unwrap();
        let text = status_report(&db, &config, t(2)).unwrap();
        assert_eq!(
            text,
            "daemon: running\n\
             me-work (me-work): last poll never, http error: timeout\n\
             notifier: notifications are off for gh-overview\n\
             acme/api#1: seen — alice requested changes\n"
        );
    }

    #[test]
    fn snapshot_report_shows_direct_plus_team_and_withheld_results() {
        let snapshot = AccountSnapshot {
            login: "me-work".into(),
            to_review: vec![ReviewRequest {
                base: base("acme/web", 2),
                direct: true,
                team: Some("fe".into()),
                event: None,
                reviews: vec![],
            }],
            withheld_mine: true,
            ..Default::default()
        };
        let text = snapshot_report(&snapshot, &identity());
        assert!(text.contains("review acme/web#2 \"PR 2\" by me-work (direct, team:fe)\n"));
        assert!(text.contains("warning: some authored PRs were withheld"));
        assert!(!text.contains("review requests were withheld"));
    }
}
