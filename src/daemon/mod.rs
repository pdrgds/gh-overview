mod run;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::{DateTime, Duration, Local, Utc};
use tracing::warn;

use crate::clock::Clock;
use crate::config::Config;
use crate::domain::activity::{ActivityItem, activity_of};
use crate::domain::actors::Identity;
use crate::domain::alert::{AlertState, Effect, Event, Phase, step};
use crate::domain::model::{AccountSnapshot, PrBase};
use crate::domain::reasons::reasons;
use crate::domain::snooze::SnoozeChoice;
use crate::github::client::GithubSource;
use crate::notify::{Delivered, Notification, Notifier, Response};
use crate::store::{Command, PrRow, Store, Tab};

pub use run::run;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    pub account: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
}

impl PrInfo {
    fn of(account: &str, b: &PrBase) -> Self {
        PrInfo {
            account: account.to_string(),
            repo: b.repo.clone(),
            number: b.number,
            title: b.title.clone(),
            url: b.url.clone(),
        }
    }
}

impl From<&PrRow> for PrInfo {
    fn from(r: &PrRow) -> Self {
        PrInfo {
            account: r.account.clone(),
            repo: r.repo.clone(),
            number: r.number,
            title: r.title.clone(),
            url: r.url.clone(),
        }
    }
}

pub type Opener = Box<dyn FnMut(&str, &str)>;

const HISTORY_SLACK: Duration = Duration::minutes(10);
const WITHHELD_GRACE: Duration = Duration::hours(1);
const STALE_ALERT: Duration = Duration::days(1);

pub struct Daemon {
    store: Store,
    github: Box<dyn GithubSource>,
    notifier: Box<dyn Notifier>,
    clock: Box<dyn Clock>,
    opener: Opener,
    config: Config,
    identity: Identity,
    snooze: Vec<SnoozeChoice>,
    next_poll: HashMap<String, DateTime<Utc>>,
    failures: HashMap<String, u32>,
    live: HashMap<String, PrInfo>,
    resync: bool,
    withheld_since: HashMap<String, DateTime<Utc>>,
    truncated: HashMap<String, Vec<String>>,
}

pub fn new_warnings<'a>(previous: &[String], current: &'a [String]) -> Vec<&'a String> {
    current.iter().filter(|w| !previous.contains(w)).collect()
}

pub fn backoff(base: Duration, failures: u32) -> Duration {
    let factor = 2i32.pow(failures.saturating_sub(1).min(8));
    (base * factor).min(Duration::minutes(5).max(base))
}

impl Daemon {
    pub fn new(
        store: Store,
        github: Box<dyn GithubSource>,
        notifier: Box<dyn Notifier>,
        clock: Box<dyn Clock>,
        opener: Opener,
        config: Config,
    ) -> Self {
        Daemon {
            identity: config.identity(),
            snooze: config.snooze(),
            store,
            github,
            notifier,
            clock,
            opener,
            config,
            next_poll: HashMap::new(),
            failures: HashMap::new(),
            live: HashMap::new(),
            resync: true,
            withheld_since: HashMap::new(),
            truncated: HashMap::new(),
        }
    }

    pub fn cycle(&mut self) -> Result<()> {
        self.heartbeat()?;
        self.consume_commands()?;
        self.poll_due();
        self.tick()
    }

    pub fn prune_unknown_accounts(&mut self) -> Result<()> {
        let logins: Vec<&str> = self.config.accounts.iter().map(|a| a.login.as_str()).collect();
        let removed = self.store.db().retain_accounts(&logins)?;
        self.store.db().retain_account_meta(&logins)?;
        if removed > 0 {
            warn!("dropped {removed} rows of accounts no longer in the config");
        }
        Ok(())
    }

    pub fn heartbeat(&mut self) -> Result<()> {
        let db = self.store.db();
        db.set_meta("heartbeat", &self.clock.now().to_rfc3339())?;
        match self.notifier.degraded() {
            Some(message) => db.set_meta("notifier_degraded", &message),
            None => db.delete_meta("notifier_degraded"),
        }
    }

    pub fn poll_due(&mut self) {
        let now = self.clock.now();
        let logins: Vec<String> = self.config.accounts.iter().map(|a| a.login.clone()).collect();
        for login in logins {
            if self.next_poll.get(&login).is_some_and(|t| *t > now) {
                continue;
            }
            if let Err(err) = self.poll_account(&login) {
                let message = format!("storing poll results failed: {err:#}");
                warn!(account = login.as_str(), "{message}");
                let _ = self.store.db().set_meta(&format!("last_error:{login}"), &message);
            }
            if let Err(err) = self.heartbeat() {
                warn!("heartbeat failed: {err:#}");
            }
        }
    }

    pub fn poll_account(&mut self, login: &str) -> Result<()> {
        let now = self.clock.now();
        match self.github.fetch(login) {
            Ok(snapshot) => {
                self.failures.remove(login);
                let mut next = now + self.config.poll();
                if let (Some(remaining), Some(reset)) = (snapshot.rate_remaining, snapshot.rate_reset_at)
                    && remaining < 100
                    && reset > next
                {
                    next = reset;
                }
                self.next_poll.insert(login.to_string(), next);
                let previous = self
                    .truncated
                    .insert(login.to_string(), snapshot.truncated.clone())
                    .unwrap_or_default();
                for warning in new_warnings(&previous, &snapshot.truncated) {
                    warn!(account = login, "{warning}");
                }
                self.apply_snapshot(snapshot, now)
            }
            Err(err) => {
                let failures = self.failures.entry(login.to_string()).or_insert(0);
                *failures += 1;
                let wait = backoff(self.config.poll(), *failures);
                self.next_poll.insert(login.to_string(), now + wait);
                warn!(account = login, "poll failed: {err}");
                self.store
                    .db()
                    .set_meta(&format!("last_error:{login}"), &err.to_string())
            }
        }
    }

    fn apply_snapshot(&mut self, snapshot: AccountSnapshot, now: DateTime<Utc>) -> Result<()> {
        let login = snapshot.login.clone();
        let identity = &self.identity;
        let renotify = self.config.renotify();
        let withheld_since = &self.withheld_since;
        let (pending, kept) = self.store.tx(|db| {
            let bootstrapped_key = format!("bootstrapped:{login}");
            let bootstrapped = db.meta(&bootstrapped_key)?.is_some();
            let history_cutoff = db
                .meta(&format!("last_poll:{login}"))?
                .and_then(|v| DateTime::parse_from_rfc3339(&v).ok())
                .map(|t| t.with_timezone(&Utc) - HISTORY_SLACK);
            let mut rows = Vec::new();
            let mut pending = Vec::new();
            for pr in &snapshot.mine {
                let key = pr.base.key();
                let pr_reasons = reasons(pr, identity);
                let items = activity_of(pr, identity);
                let untackled = !pr_reasons.is_empty();
                if untackled {
                    rows.push(PrRow::mine(&login, pr, pr_reasons, items.last().map(|i| i.at)));
                }
                let known = db.has_activity_for(&key)?;
                db.mark_seen(&key, &format!("pr:{key}"), now)?;
                let mut fresh: Vec<ActivityItem> = Vec::new();
                for item in items {
                    if db.is_seen(&item.id)? {
                        continue;
                    }
                    db.mark_seen(&key, &item.id, now)?;
                    if known || history_cutoff.is_none_or(|cutoff| item.at >= cutoff) {
                        fresh.push(item);
                    }
                }
                if fresh.is_empty() || !bootstrapped {
                    continue;
                }
                let state = db.alert(&key)?;
                let (next, effects) = step(
                    &state,
                    Event::NewActivity {
                        items: fresh,
                        untackled,
                    },
                    now,
                    renotify,
                );
                db.put_alert(&key, &next)?;
                pending.push((key, PrInfo::of(&login, &pr.base), effects));
            }
            for request in &snapshot.to_review {
                rows.push(PrRow::review(&login, request));
            }
            let mut kept = Vec::new();
            let searches = [
                (
                    Tab::Mine,
                    snapshot.withheld_mine,
                    snapshot.mine.iter().map(|pr| pr.base.key()).collect::<HashSet<_>>(),
                ),
                (
                    Tab::Review,
                    snapshot.withheld_review,
                    snapshot.to_review.iter().map(|r| r.base.key()).collect(),
                ),
            ];
            for (tab, withheld, present) in searches {
                if !withheld {
                    continue;
                }
                for row in db.prs(tab)? {
                    let id = format!("{login} {} {}", tab.as_str(), row.key);
                    let fresh_enough = withheld_since
                        .get(&id)
                        .is_none_or(|since| now - *since < WITHHELD_GRACE);
                    if row.account == login && !present.contains(&row.key) && fresh_enough {
                        kept.push(id);
                        rows.push(row);
                    }
                }
            }
            db.replace_account_prs(&login, &rows)?;
            db.set_meta(&format!("last_poll:{login}"), &now.to_rfc3339())?;
            if snapshot.errors.is_empty() {
                db.delete_meta(&format!("last_error:{login}"))?;
            } else {
                db.set_meta(&format!("last_error:{login}"), &snapshot.errors.join("; "))?;
            }
            if !bootstrapped {
                db.set_meta(&bootstrapped_key, &now.to_rfc3339())?;
            }
            Ok((pending, kept))
        })?;
        let prefix = format!("{login} ");
        self.withheld_since
            .retain(|id, _| !id.starts_with(&prefix) || kept.contains(id));
        for id in kept {
            self.withheld_since.entry(id).or_insert(now);
        }
        for (key, info, effects) in pending {
            self.apply_effects(&key, Some(info), effects);
        }
        Ok(())
    }

    pub fn tick(&mut self) -> Result<()> {
        let now = self.clock.now();
        let renotify = self.config.renotify();
        self.absorb_notifier_reset();
        let alerts = self.store.db().alerts()?;
        let resync = std::mem::take(&mut self.resync);
        for (key, state) in alerts {
            let row = self.store.db().pr(Tab::Mine, &key)?;
            let mut current = state.clone();
            if resync && state.phase(now) == Phase::Pinging && !self.live.contains_key(&key) {
                current.last_notified_at = None;
            }
            let (next, effects) = step(
                &current,
                Event::Tick {
                    untackled: row.is_some(),
                },
                now,
                renotify,
            );
            let stale = row.is_none()
                && next.phase(now) == Phase::Idle
                && next.last_notified_at.is_none_or(|t| now - t > STALE_ALERT);
            if stale {
                self.store.db().put_alert(&key, &AlertState::default())?;
                self.live.remove(&key);
            } else if next != state {
                self.store.db().put_alert(&key, &next)?;
            }
            self.apply_effects(&key, row.as_ref().map(PrInfo::from), effects);
        }
        Ok(())
    }

    pub fn handle(&mut self, delivered: Delivered) -> Result<()> {
        let now = self.clock.now();
        let event = match delivered.response {
            Response::Opened => Event::Opened {
                generation: delivered.generation,
            },
            Response::Snoozed(label) => {
                let hour = self.config.tomorrow_hour;
                let Some(choice) = self.snooze.iter().find(|c| c.label(hour) == label) else {
                    warn!("unknown snooze choice {label:?}");
                    return Ok(());
                };
                Event::SnoozedFromNotification {
                    generation: delivered.generation,
                    until: choice.until(now, &Local, hour),
                }
            }
            Response::Closed => return Ok(()),
        };
        self.apply_event(&delivered.pr_key, event)
    }

    pub fn consume_commands(&mut self) -> Result<()> {
        let commands = self.store.tx(|db| db.take_commands())?;
        for command in commands {
            let result = match command {
                Command::Refresh => {
                    self.next_poll.clear();
                    Ok(())
                }
                Command::Ack { pr_key } => self.apply_event(&pr_key, Event::Ack),
                Command::Snooze { pr_key, until } => self.apply_event(&pr_key, Event::Snooze { until }),
                Command::Done { pr_key } => self.apply_event(&pr_key, Event::Done),
            };
            if let Err(err) = result {
                warn!("applying a TUI command failed: {err:#}");
            }
        }
        Ok(())
    }

    fn apply_event(&mut self, key: &str, event: Event) -> Result<()> {
        let now = self.clock.now();
        let state = self.store.db().alert(key)?;
        let (next, effects) = step(&state, event, now, self.config.renotify());
        if next != state {
            self.store.db().put_alert(key, &next)?;
        }
        self.apply_effects(key, None, effects);
        Ok(())
    }

    fn absorb_notifier_reset(&mut self) {
        if self.notifier.take_reset() {
            self.live.clear();
            self.resync = true;
        }
    }

    fn info_for(&self, key: &str, info: Option<PrInfo>) -> Option<PrInfo> {
        info.or_else(|| self.live.get(key).cloned()).or_else(|| {
            self.store
                .db()
                .pr(Tab::Mine, key)
                .ok()
                .flatten()
                .as_ref()
                .map(PrInfo::from)
        })
    }

    fn apply_effects(&mut self, key: &str, info: Option<PrInfo>, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Notify {
                    generation,
                    summary,
                    snoozable,
                } => {
                    let Some(info) = self.info_for(key, info.clone()) else {
                        warn!("no PR details for {key}; notification skipped");
                        continue;
                    };
                    let notification = Notification {
                        pr_key: key.to_string(),
                        generation,
                        title: format!("{} #{}", info.repo, info.number),
                        subtitle: info.title.clone(),
                        message: summary,
                        snoozable,
                    };
                    if let Err(err) = self.notifier.show(&notification) {
                        warn!("notify {key} failed: {err:#}");
                    }
                    self.absorb_notifier_reset();
                    self.live.insert(key.to_string(), info);
                }
                Effect::Remove => {
                    if let Err(err) = self.notifier.remove(key) {
                        warn!("remove {key} failed: {err:#}");
                    }
                    self.absorb_notifier_reset();
                    self.live.remove(key);
                }
                Effect::OpenUrl => match self.info_for(key, info.clone()) {
                    Some(info) => (self.opener)(&info.account, &info.url),
                    None => warn!("no PR details for {key}; cannot open it"),
                },
            }
        }
    }
}
