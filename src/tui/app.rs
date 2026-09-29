use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::{DateTime, Duration, Local, Utc};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::status::{HeaderStatus, ago, header_status};
use crate::config::Config;
use crate::domain::alert::{AlertState, Event, Phase, step};
use crate::domain::reasons::describe;
use crate::domain::snooze::SnoozeChoice;
use crate::store::{Command, Db, PrRow, Tab};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Badge {
    None,
    Pinging,
    Snoozed(DateTime<Utc>),
    Seen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewRow {
    pub key: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub age: String,
    pub login: String,
    pub account: String,
    pub team: Option<String>,
    pub why: String,
    pub badge: Badge,
    pub is_draft: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Open { login: String, url: String },
    Enqueue(Command),
}

const PREDICTION_TTL: Duration = Duration::seconds(15);

struct Prediction {
    base: AlertState,
    state: AlertState,
    at: DateTime<Utc>,
}

pub struct App {
    pub tab: Tab,
    pub review: Vec<ViewRow>,
    pub mine: Vec<ViewRow>,
    pub status: HeaderStatus,
    pub snoozing: bool,
    pub quit: bool,
    selected: HashMap<Tab, String>,
    hidden: HashSet<String>,
    snooze: Vec<SnoozeChoice>,
    tomorrow_hour: u32,
    renotify: Duration,
    remind_after_open: Duration,
    alerts: HashMap<String, AlertState>,
    predictions: HashMap<String, Prediction>,
}

fn view_row(row: PrRow, config: &Config, now: DateTime<Utc>, badge: Badge) -> ViewRow {
    ViewRow {
        age: ago(now - row.created_at),
        account: config.label_for(&row.account).to_string(),
        login: row.account,
        why: describe(&row.reasons),
        key: row.key,
        repo: row.repo,
        number: row.number,
        title: row.title,
        url: row.url,
        author: row.author,
        team: row.team,
        badge,
        is_draft: row.is_draft,
    }
}

fn visible_badge(
    key: &str,
    alerts: &HashMap<String, AlertState>,
    hidden: &HashSet<String>,
    now: DateTime<Utc>,
) -> Option<Badge> {
    let state = alerts.get(key).cloned().unwrap_or_default();
    let phase = state.phase(now);
    if state.done_at.is_some() || (phase == Phase::Idle && hidden.contains(key)) {
        return None;
    }
    Some(match phase {
        Phase::Idle => Badge::None,
        Phase::Pinging => Badge::Pinging,
        Phase::Snoozed => Badge::Snoozed(state.snoozed_until.expect("snoozed has until")),
        Phase::Acked => Badge::Seen,
    })
}

pub fn review_rows(
    rows: Vec<PrRow>,
    alerts: &HashMap<String, AlertState>,
    hidden: &HashSet<String>,
    config: &Config,
    now: DateTime<Utc>,
) -> Vec<ViewRow> {
    let mut by_key: HashMap<String, PrRow> = HashMap::new();
    for row in rows {
        let keep = by_key
            .get(&row.key)
            .is_none_or(|existing| !existing.is_direct && row.is_direct);
        if keep {
            by_key.insert(row.key.clone(), row);
        }
    }
    let mut rows: Vec<(PrRow, Badge)> = by_key
        .into_values()
        .filter_map(|r| visible_badge(&r.key, alerts, hidden, now).map(|badge| (r, badge)))
        .collect();
    rows.sort_by(|(a, ab), (b, bb)| {
        (*ab != Badge::Pinging, !a.is_direct, a.created_at, &a.key).cmp(&(
            *bb != Badge::Pinging,
            !b.is_direct,
            b.created_at,
            &b.key,
        ))
    });
    rows.into_iter()
        .map(|(r, badge)| view_row(r, config, now, badge))
        .collect()
}

pub fn mine_rows(
    rows: Vec<PrRow>,
    alerts: &HashMap<String, AlertState>,
    hidden: &HashSet<String>,
    config: &Config,
    now: DateTime<Utc>,
) -> Vec<ViewRow> {
    let mut rows: Vec<(PrRow, Badge)> = rows
        .into_iter()
        .filter_map(|r| visible_badge(&r.key, alerts, hidden, now).map(|badge| (r, badge)))
        .collect();
    rows.sort_by(|(a, ab), (b, bb)| {
        (*ab != Badge::Pinging, std::cmp::Reverse(a.last_activity_at), &a.key).cmp(&(
            *bb != Badge::Pinging,
            std::cmp::Reverse(b.last_activity_at),
            &b.key,
        ))
    });
    rows.into_iter()
        .map(|(r, badge)| view_row(r, config, now, badge))
        .collect()
}

impl App {
    pub fn new(config: &Config) -> Self {
        App {
            tab: Tab::Review,
            review: vec![],
            mine: vec![],
            status: HeaderStatus::default(),
            snoozing: false,
            quit: false,
            selected: HashMap::new(),
            hidden: HashSet::new(),
            snooze: config.snooze(),
            tomorrow_hour: config.tomorrow_hour,
            renotify: config.renotify(),
            remind_after_open: config.remind_after_open(),
            alerts: HashMap::new(),
            predictions: HashMap::new(),
        }
    }

    pub fn load(&mut self, db: &Db<'_>, config: &Config, now: DateTime<Utc>) -> Result<()> {
        self.alerts = db.alerts()?.into_iter().collect();
        let stored = &self.alerts;
        self.predictions
            .retain(|key, p| stored.get(key).cloned().unwrap_or_default() == p.base && now - p.at < PREDICTION_TTL);
        let mut alerts = self.alerts.clone();
        for (key, p) in &self.predictions {
            alerts.insert(key.clone(), p.state.clone());
        }
        self.hidden.retain(|key| {
            alerts
                .get(key)
                .is_none_or(|a| a.done_at.is_none() && a.phase(now) == Phase::Idle)
        });
        self.review = review_rows(db.prs(Tab::Review)?, &alerts, &self.hidden, config, now);
        self.mine = mine_rows(db.prs(Tab::Mine)?, &alerts, &self.hidden, config, now);
        self.status = header_status(db, config, now)?;
        Ok(())
    }

    pub fn rows(&self) -> &[ViewRow] {
        match self.tab {
            Tab::Review => &self.review,
            Tab::Mine => &self.mine,
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        let rows = self.rows();
        if rows.is_empty() {
            return None;
        }
        let index = self
            .selected
            .get(&self.tab)
            .and_then(|key| rows.iter().position(|r| &r.key == key));
        Some(index.unwrap_or(0))
    }

    pub fn selected_row(&self) -> Option<&ViewRow> {
        self.selected_index().map(|i| &self.rows()[i])
    }

    pub fn snooze_labels(&self) -> Vec<String> {
        self.snooze.iter().map(|c| c.label(self.tomorrow_hour)).collect()
    }

    fn predict(&mut self, key: &str, event: Event, now: DateTime<Utc>) {
        let base = self.alerts.get(key).cloned().unwrap_or_default();
        let (base, from) = match self.predictions.remove(key) {
            Some(p) => (p.base, p.state),
            None => (base.clone(), base),
        };
        let (state, _) = step(&from, event, now, self.renotify);
        self.predictions
            .insert(key.to_string(), Prediction { base, state, at: now });
    }

    fn select(&mut self, index: usize) {
        if let Some(row) = self.rows().get(index) {
            let key = row.key.clone();
            self.selected.insert(self.tab, key);
        }
    }

    fn move_by(&mut self, delta: isize) {
        let Some(current) = self.selected_index() else { return };
        let last = self.rows().len() as isize - 1;
        self.select((current as isize + delta).clamp(0, last) as usize);
    }

    pub fn on_key(&mut self, key: KeyEvent, now: DateTime<Utc>) -> Vec<Action> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return Vec::new();
        }
        if self.snoozing {
            return self.on_snooze_key(key, now);
        }
        let mut actions = Vec::new();
        let selected = self.selected_row().cloned();
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('1') => self.tab = Tab::Review,
            KeyCode::Char('2') => self.tab = Tab::Mine,
            KeyCode::Tab => {
                self.tab = if self.tab == Tab::Review {
                    Tab::Mine
                } else {
                    Tab::Review
                };
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Char('g') => self.select(0),
            KeyCode::Char('G') => self.select(self.rows().len().saturating_sub(1)),
            KeyCode::Enter | KeyCode::Char('o') => {
                if let Some(row) = selected {
                    actions.push(Action::Open {
                        login: row.login.clone(),
                        url: row.url.clone(),
                    });
                    if self.tab == Tab::Mine || row.badge != Badge::None {
                        let remind_at = now + self.remind_after_open;
                        self.predict(&row.key, Event::Ack { remind_at }, now);
                        actions.push(Action::Enqueue(Command::Ack { pr_key: row.key }));
                    }
                }
            }
            KeyCode::Char('s') => {
                if selected.is_some_and(|r| r.badge != Badge::None) {
                    self.snoozing = true;
                }
            }
            KeyCode::Char('d') => {
                if let Some(row) = selected {
                    self.predict(&row.key, Event::Done, now);
                    self.hidden.insert(row.key.clone());
                    self.mine.retain(|r| r.key != row.key);
                    self.review.retain(|r| r.key != row.key);
                    actions.push(Action::Enqueue(Command::Done { pr_key: row.key }));
                }
            }
            KeyCode::Char('r') => actions.push(Action::Enqueue(Command::Refresh)),
            _ => {}
        }
        actions
    }

    fn on_snooze_key(&mut self, key: KeyEvent, now: DateTime<Utc>) -> Vec<Action> {
        let mut actions = Vec::new();
        if let KeyCode::Char(c) = key.code
            && let Some(choice) = c
                .to_digit(10)
                .and_then(|d| self.snooze.get((d as usize).wrapping_sub(1)))
            && let Some(row) = self.selected_row()
        {
            let until = choice.until(now, &Local, self.tomorrow_hour);
            let pr_key = row.key.clone();
            self.predict(&pr_key, Event::Snooze { until }, now);
            actions.push(Action::Enqueue(Command::Snooze { pr_key, until }));
        }
        if matches!(key.code, KeyCode::Esc | KeyCode::Char(_)) {
            self.snoozing = false;
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;
    use crate::domain::reasons::Reason;
    use crate::domain::testkit::*;

    fn config() -> Config {
        let mut c = Config::with_accounts(vec!["me-work".into(), "me-home".into()]);
        c.accounts[0].label = "work".into();
        c.accounts[1].label = "personal".into();
        c.snooze_choices = vec!["15m".into(), "1h".into()];
        c
    }

    fn row(tab: Tab, account: &str, repo: &str, number: u64) -> PrRow {
        let mut r = PrRow::mine(account, &my_pr(repo, number), vec![Reason::Threads { count: 1 }], None);
        r.tab = tab;
        r
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn review_rows_dedupe_prefer_direct_and_sort_direct_then_oldest() {
        let mut team = row(Tab::Review, "me-work", "acme/a", 1);
        team.is_direct = false;
        team.team = Some("platform".into());
        let mut direct_dup = row(Tab::Review, "me-home", "acme/a", 1);
        direct_dup.is_direct = true;
        let mut old_team = row(Tab::Review, "me-work", "acme/b", 2);
        old_team.is_direct = false;
        old_team.created_at = t(-5000);
        let mut new_direct = row(Tab::Review, "me-work", "acme/c", 3);
        new_direct.created_at = t(-10);
        let rows = review_rows(
            vec![team, direct_dup, old_team, new_direct],
            &HashMap::new(),
            &HashSet::new(),
            &config(),
            t(0),
        );
        let keys: Vec<(&str, &str)> = rows.iter().map(|r| (r.key.as_str(), r.account.as_str())).collect();
        assert_eq!(
            keys,
            vec![("acme/a#1", "personal"), ("acme/c#3", "work"), ("acme/b#2", "work")]
        );
    }

    #[test]
    fn mine_rows_hide_done_and_put_pinging_first() {
        let mut quiet = row(Tab::Mine, "me-work", "acme/a", 1);
        quiet.last_activity_at = Some(t(9));
        let pinging = row(Tab::Mine, "me-work", "acme/b", 2);
        let done = row(Tab::Mine, "me-work", "acme/c", 3);
        let alerts = HashMap::from([
            (
                "acme/b#2".to_string(),
                AlertState {
                    cycle_started_at: Some(t(0)),
                    ..Default::default()
                },
            ),
            (
                "acme/c#3".to_string(),
                AlertState {
                    done_at: Some(t(0)),
                    ..Default::default()
                },
            ),
        ]);
        let rows = mine_rows(vec![quiet, pinging, done], &alerts, &HashSet::new(), &config(), t(1));
        let view: Vec<(&str, &Badge)> = rows.iter().map(|r| (r.key.as_str(), &r.badge)).collect();
        assert_eq!(view, vec![("acme/b#2", &Badge::Pinging), ("acme/a#1", &Badge::None)]);
        assert_eq!(rows[0].why, "1 thread");
    }

    fn app_with_mine() -> App {
        let mut app = App::new(&config());
        app.tab = Tab::Mine;
        let alerts = HashMap::from([(
            "acme/a#1".to_string(),
            AlertState {
                cycle_started_at: Some(t(0)),
                ..Default::default()
            },
        )]);
        app.mine = mine_rows(
            vec![
                row(Tab::Mine, "me-work", "acme/a", 1),
                row(Tab::Mine, "me-work", "acme/b", 2),
            ],
            &alerts,
            &HashSet::new(),
            &config(),
            t(0),
        );
        app
    }

    #[test]
    fn navigation_keeps_selection_by_key() {
        let mut app = app_with_mine();
        assert_eq!(app.selected_row().unwrap().key, "acme/a#1");
        app.on_key(key(KeyCode::Char('j')), t(0));
        app.on_key(key(KeyCode::Char('j')), t(0));
        assert_eq!(app.selected_row().unwrap().key, "acme/b#2");
        app.mine.reverse();
        assert_eq!(app.selected_index(), Some(0));
        app.on_key(key(KeyCode::Char('G')), t(0));
        assert_eq!(app.selected_row().unwrap().key, "acme/a#1");
    }

    #[test]
    fn enter_on_mine_opens_and_acks() {
        let mut app = app_with_mine();
        assert_eq!(
            app.on_key(key(KeyCode::Enter), t(0)),
            vec![
                Action::Open {
                    login: "me-work".into(),
                    url: "https://github.com/acme/a/pull/1".into()
                },
                Action::Enqueue(Command::Ack {
                    pr_key: "acme/a#1".into()
                }),
            ]
        );
    }

    #[test]
    fn snooze_popup_only_for_active_rows_and_enqueues_choice() {
        let mut app = app_with_mine();
        app.on_key(key(KeyCode::Char('s')), t(0));
        assert!(app.snoozing);
        let actions = app.on_key(key(KeyCode::Char('2')), t(0));
        assert_eq!(
            actions,
            vec![Action::Enqueue(Command::Snooze {
                pr_key: "acme/a#1".into(),
                until: t(0) + Duration::hours(1)
            })]
        );
        assert!(!app.snoozing);
        app.on_key(key(KeyCode::Char('j')), t(0));
        app.on_key(key(KeyCode::Char('s')), t(0));
        assert!(!app.snoozing);
    }

    #[test]
    fn done_hides_row_immediately_and_enqueues() {
        let mut app = app_with_mine();
        let actions = app.on_key(key(KeyCode::Char('d')), t(0));
        assert_eq!(
            actions,
            vec![Action::Enqueue(Command::Done {
                pr_key: "acme/a#1".into()
            })]
        );
        assert_eq!(app.mine.len(), 1);
    }

    #[test]
    fn tabs_refresh_and_quit() {
        let mut app = App::new(&config());
        app.on_key(key(KeyCode::Tab), t(0));
        assert_eq!(app.tab, Tab::Mine);
        app.on_key(key(KeyCode::Char('1')), t(0));
        assert_eq!(app.tab, Tab::Review);
        assert_eq!(
            app.on_key(key(KeyCode::Char('r')), t(0)),
            vec![Action::Enqueue(Command::Refresh)]
        );
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), t(0));
        assert!(app.quit);
    }

    #[test]
    fn badges_follow_the_alert_phase() {
        let alerts = HashMap::from([
            (
                "acme/a#1".to_string(),
                AlertState {
                    cycle_started_at: Some(t(0)),
                    snoozed_until: Some(t(30)),
                    ..Default::default()
                },
            ),
            (
                "acme/b#2".to_string(),
                AlertState {
                    cycle_started_at: Some(t(0)),
                    acked_at: Some(t(1)),
                    ..Default::default()
                },
            ),
        ]);
        let rows = mine_rows(
            vec![
                row(Tab::Mine, "me-work", "acme/a", 1),
                row(Tab::Mine, "me-work", "acme/b", 2),
            ],
            &alerts,
            &HashSet::new(),
            &config(),
            t(2),
        );
        let badges: Vec<&Badge> = rows.iter().map(|r| &r.badge).collect();
        assert_eq!(badges, vec![&Badge::Snoozed(t(30)), &Badge::Seen]);
    }

    #[test]
    fn local_hiding_only_applies_to_idle_rows() {
        let alerts = HashMap::from([(
            "acme/b#2".to_string(),
            AlertState {
                cycle_started_at: Some(t(0)),
                ..Default::default()
            },
        )]);
        let hidden = HashSet::from(["acme/a#1".to_string(), "acme/b#2".to_string()]);
        let rows = mine_rows(
            vec![
                row(Tab::Mine, "me-work", "acme/a", 1),
                row(Tab::Mine, "me-work", "acme/b", 2),
            ],
            &alerts,
            &hidden,
            &config(),
            t(1),
        );
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, vec!["acme/b#2"]);
    }

    #[test]
    fn a_locally_hidden_row_comes_back_when_its_alert_is_active_again() {
        let store = crate::store::Store::open_in_memory().unwrap();
        let db = store.db();
        db.replace_account_prs("me-work", &[row(Tab::Mine, "me-work", "acme/a", 1)])
            .unwrap();
        let mut app = App::new(&config());
        app.tab = Tab::Mine;
        app.load(&db, &config(), t(0)).unwrap();
        app.on_key(key(KeyCode::Char('d')), t(0));
        app.load(&db, &config(), t(0)).unwrap();
        assert!(app.mine.is_empty());
        db.put_alert(
            "acme/a#1",
            &AlertState {
                cycle_started_at: Some(t(1)),
                ..Default::default()
            },
        )
        .unwrap();
        app.load(&db, &config(), t(2)).unwrap();
        assert_eq!(app.mine.len(), 1);
        assert_eq!(app.mine[0].badge, Badge::Pinging);
        db.put_alert("acme/a#1", &AlertState::default()).unwrap();
        app.load(&db, &config(), t(3)).unwrap();
        assert_eq!(app.mine.len(), 1);
    }

    fn store_with_a_pinging_pr() -> crate::store::Store {
        let store = crate::store::Store::open_in_memory().unwrap();
        let db = store.db();
        db.replace_account_prs("me-work", &[row(Tab::Mine, "me-work", "acme/a", 1)])
            .unwrap();
        db.put_alert(
            "acme/a#1",
            &AlertState {
                cycle_started_at: Some(t(0)),
                ..Default::default()
            },
        )
        .unwrap();
        store
    }

    fn loaded_on_mine(db: &Db<'_>) -> App {
        let mut app = App::new(&config());
        app.tab = Tab::Mine;
        app.load(db, &config(), t(0)).unwrap();
        assert_eq!(app.mine[0].badge, Badge::Pinging);
        app
    }

    #[test]
    fn a_snooze_shows_at_once_then_follows_the_daemon() {
        let store = store_with_a_pinging_pr();
        let db = store.db();
        let mut app = loaded_on_mine(&db);
        app.on_key(key(KeyCode::Char('s')), t(0));
        app.on_key(key(KeyCode::Char('1')), t(0));
        app.load(&db, &config(), t(0)).unwrap();
        assert_eq!(app.mine[0].badge, Badge::Snoozed(t(15)));
        db.put_alert(
            "acme/a#1",
            &AlertState {
                cycle_started_at: Some(t(0)),
                snoozed_until: Some(t(16)),
                ..Default::default()
            },
        )
        .unwrap();
        app.load(&db, &config(), t(0)).unwrap();
        assert_eq!(app.mine[0].badge, Badge::Snoozed(t(16)));
    }

    #[test]
    fn opening_a_pinging_pr_shows_it_seen_at_once() {
        let store = store_with_a_pinging_pr();
        let db = store.db();
        let mut app = loaded_on_mine(&db);
        app.on_key(key(KeyCode::Enter), t(0));
        app.load(&db, &config(), t(0)).unwrap();
        assert_eq!(app.mine[0].badge, Badge::Seen);
    }

    #[test]
    fn a_prediction_the_daemon_never_confirms_expires() {
        let store = store_with_a_pinging_pr();
        let db = store.db();
        let mut app = loaded_on_mine(&db);
        app.on_key(key(KeyCode::Char('s')), t(0));
        app.on_key(key(KeyCode::Char('2')), t(0));
        app.load(&db, &config(), t(0) + Duration::seconds(14)).unwrap();
        assert_eq!(app.mine[0].badge, Badge::Snoozed(t(60)));
        app.load(&db, &config(), t(0) + Duration::seconds(15)).unwrap();
        assert_eq!(app.mine[0].badge, Badge::Pinging);
    }

    #[test]
    fn a_pinging_review_request_can_be_opened_snoozed_and_dismissed() {
        let store = crate::store::Store::open_in_memory().unwrap();
        let db = store.db();
        let mut requested = row(Tab::Review, "me-work", "acme/r", 9);
        requested.is_direct = true;
        db.replace_account_prs("me-work", &[requested]).unwrap();
        db.put_alert(
            "acme/r#9",
            &AlertState {
                cycle_started_at: Some(t(0)),
                ..Default::default()
            },
        )
        .unwrap();
        let mut app = App::new(&config());
        app.load(&db, &config(), t(0)).unwrap();
        assert_eq!(app.review[0].badge, Badge::Pinging);
        assert_eq!(
            app.on_key(key(KeyCode::Enter), t(0))[1],
            Action::Enqueue(Command::Ack {
                pr_key: "acme/r#9".into()
            })
        );
        app.load(&db, &config(), t(0)).unwrap();
        assert_eq!(app.review[0].badge, Badge::Seen);
        app.on_key(key(KeyCode::Char('s')), t(0));
        assert!(app.snoozing);
        app.on_key(key(KeyCode::Esc), t(0));
        assert_eq!(
            app.on_key(key(KeyCode::Char('d')), t(0)),
            vec![Action::Enqueue(Command::Done {
                pr_key: "acme/r#9".into()
            })]
        );
        assert!(app.review.is_empty());
        app.load(&db, &config(), t(0)).unwrap();
        assert!(app.review.is_empty());
    }

    #[test]
    fn ctrl_c_quits_even_with_the_snooze_popup_open() {
        let mut app = app_with_mine();
        app.on_key(key(KeyCode::Char('s')), t(0));
        assert!(app.snoozing);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), t(0));
        assert!(app.quit);
    }

    #[test]
    fn enter_on_review_opens_without_acknowledging() {
        let mut app = App::new(&config());
        app.review = review_rows(
            vec![row(Tab::Review, "me-work", "acme/a", 1)],
            &HashMap::new(),
            &HashSet::new(),
            &config(),
            t(0),
        );
        assert_eq!(
            app.on_key(key(KeyCode::Enter), t(0)),
            vec![Action::Open {
                login: "me-work".into(),
                url: "https://github.com/acme/a/pull/1".into()
            }]
        );
    }
}
