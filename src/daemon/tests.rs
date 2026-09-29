use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use chrono::Duration;

use super::*;
use crate::clock::FakeClock;
use crate::domain::model::{Author, MyPr, RequestEvent, ReviewMark, ReviewState};
use crate::domain::reasons::Reason;
use crate::domain::testkit::*;
use crate::github::FetchError;

#[derive(Clone, Default)]
struct FakeGithub(Rc<RefCell<VecDeque<Result<AccountSnapshot, FetchError>>>>);

impl FakeGithub {
    fn push(&self, result: Result<AccountSnapshot, FetchError>) {
        self.0.borrow_mut().push_back(result);
    }
}

impl GithubSource for FakeGithub {
    fn fetch(&self, _login: &str) -> Result<AccountSnapshot, FetchError> {
        self.0.borrow_mut().pop_front().expect("unexpected fetch")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Show(String, u64, String, bool),
    Remove(String),
}

#[derive(Clone, Default)]
struct FakeNotifier {
    calls: Rc<RefCell<Vec<Call>>>,
    reset: Rc<Cell<bool>>,
    reset_on_show: Rc<Cell<bool>>,
}

impl FakeNotifier {
    fn take(&self) -> Vec<Call> {
        std::mem::take(&mut *self.calls.borrow_mut())
    }
}

impl Notifier for FakeNotifier {
    fn show(&mut self, n: &Notification) -> anyhow::Result<()> {
        if self.reset_on_show.replace(false) {
            self.reset.set(true);
        }
        self.calls.borrow_mut().push(Call::Show(
            n.pr_key.clone(),
            n.generation,
            n.message.clone(),
            n.snoozable,
        ));
        Ok(())
    }

    fn remove(&mut self, pr_key: &str) -> anyhow::Result<()> {
        self.calls.borrow_mut().push(Call::Remove(pr_key.to_string()));
        Ok(())
    }

    fn degraded(&self) -> Option<String> {
        None
    }

    fn take_reset(&mut self) -> bool {
        self.reset.replace(false)
    }
}

struct Harness {
    daemon: Daemon,
    github: FakeGithub,
    notifier: FakeNotifier,
    clock: FakeClock,
    opened: Rc<RefCell<Vec<String>>>,
}

fn harness() -> Harness {
    harness_with(Store::open_in_memory().unwrap())
}

fn harness_with(store: Store) -> Harness {
    harness_for(store, &["me-work"])
}

fn harness_for(store: Store, logins: &[&str]) -> Harness {
    let github = FakeGithub::default();
    let notifier = FakeNotifier::default();
    let clock = FakeClock::at(t(0));
    let opened = Rc::new(RefCell::new(Vec::new()));
    let sink = opened.clone();
    let mut config = Config::with_accounts(logins.iter().map(|l| l.to_string()).collect());
    config.snooze_choices = vec!["15m".into(), "1h".into()];
    let daemon = Daemon::new(
        store,
        Box::new(github.clone()),
        Box::new(notifier.clone()),
        Box::new(clock.clone()),
        Box::new(move |account: &str, url: &str| sink.borrow_mut().push(format!("{account} {url}"))),
        config,
    );
    Harness {
        daemon,
        github,
        notifier,
        clock,
        opened,
    }
}

fn snapshot(mine: Vec<MyPr>) -> AccountSnapshot {
    AccountSnapshot {
        login: "me-work".into(),
        mine,
        ..Default::default()
    }
}

fn request(repo: &str, number: u64) -> ReviewRequest {
    let mut base = base(repo, number);
    base.author = "dave".into();
    ReviewRequest {
        base,
        direct: true,
        team: None,
        event: Some(RequestEvent {
            id: format!("RRE_{number}"),
            actor: "dave".into(),
        }),
        reviews: vec![],
    }
}

fn re_requested(repo: &str, number: u64, by: &str) -> ReviewRequest {
    ReviewRequest {
        event: Some(RequestEvent {
            id: format!("RRE_{number}_{by}"),
            actor: by.into(),
        }),
        ..request(repo, number)
    }
}

fn team_request(repo: &str, number: u64) -> ReviewRequest {
    ReviewRequest {
        direct: false,
        team: Some("platform".into()),
        event: Some(RequestEvent {
            id: format!("RRE_{number}_team"),
            actor: "dave".into(),
        }),
        ..request(repo, number)
    }
}

fn shown_keys(calls: Vec<Call>) -> Vec<(String, u64, String)> {
    calls
        .into_iter()
        .filter_map(|call| match call {
            Call::Show(key, generation, message, _) => Some((key, generation, message)),
            Call::Remove(_) => None,
        })
        .collect()
}

fn pr_with_changes_on(repo: &str, number: u64, id: &str) -> MyPr {
    let mut pr = my_pr(repo, number);
    pr.reviews = vec![review(
        id,
        Author::user("alice"),
        ReviewState::ChangesRequested,
        0,
        "head",
    )];
    pr
}

fn pr_with_changes(ids: &[&str]) -> MyPr {
    let mut pr = my_pr("acme/api", 1);
    pr.reviews = ids
        .iter()
        .map(|id| review(id, Author::user("alice"), ReviewState::ChangesRequested, 0, "head"))
        .collect();
    pr
}

impl Harness {
    fn poll(&mut self, mine: Vec<MyPr>) {
        self.github.push(Ok(snapshot(mine)));
        self.daemon.poll_account("me-work").unwrap();
    }

    fn poll_requests(&mut self, to_review: Vec<ReviewRequest>) {
        self.github.push(Ok(AccountSnapshot {
            to_review,
            ..snapshot(vec![])
        }));
        self.daemon.poll_account("me-work").unwrap();
    }

    fn bootstrapped(mut self) -> Self {
        self.poll(vec![]);
        self.daemon.tick().unwrap();
        self
    }
}

#[test]
fn first_poll_records_activity_silently() {
    let mut h = harness();
    h.poll(vec![pr_with_changes(&["r1"])]);
    assert!(h.notifier.take().is_empty());
    assert_eq!(h.daemon.store.db().prs(Tab::Mine).unwrap().len(), 1);
    h.poll(vec![pr_with_changes(&["r1"])]);
    assert!(h.notifier.take().is_empty());
}

#[test]
fn new_review_notifies_then_renotifies_every_interval() {
    let mut h = harness().bootstrapped();
    h.poll(vec![pr_with_changes(&["r1"])]);
    assert_eq!(
        h.notifier.take(),
        vec![Call::Show(
            "acme/api#1".into(),
            1,
            "alice requested changes".into(),
            true
        )]
    );
    h.clock.advance(Duration::minutes(4));
    h.daemon.tick().unwrap();
    assert!(h.notifier.take().is_empty());
    h.clock.advance(Duration::minutes(1));
    h.daemon.tick().unwrap();
    assert_eq!(
        h.notifier.take(),
        vec![Call::Show(
            "acme/api#1".into(),
            2,
            "alice requested changes".into(),
            true
        )]
    );
}

#[test]
fn approval_with_nothing_to_tackle_notifies_once_without_snooze() {
    let mut h = harness().bootstrapped();
    let mut pr = my_pr("acme/api", 1);
    pr.reviews = vec![review("r1", Author::user("alice"), ReviewState::Approved, 0, "head")];
    h.poll(vec![pr]);
    assert_eq!(
        h.notifier.take(),
        vec![Call::Show("acme/api#1".into(), 1, "alice approved".into(), false)]
    );
    h.clock.advance(Duration::minutes(30));
    h.daemon.tick().unwrap();
    assert!(h.notifier.take().is_empty());
}

#[test]
fn clicking_the_notification_opens_and_pauses_pings_until_the_reminder() {
    let mut h = harness().bootstrapped();
    h.poll(vec![pr_with_changes(&["r1"])]);
    h.notifier.take();
    h.daemon
        .handle(Delivered {
            pr_key: "acme/api#1".into(),
            generation: 1,
            response: Response::Opened,
        })
        .unwrap();
    assert_eq!(
        *h.opened.borrow(),
        vec!["me-work https://github.com/acme/api/pull/1".to_string()]
    );
    h.clock.advance(Duration::minutes(29));
    h.daemon.tick().unwrap();
    assert!(h.notifier.take().is_empty());
    h.clock.advance(Duration::minutes(1));
    h.daemon.tick().unwrap();
    assert_eq!(
        h.notifier.take(),
        vec![Call::Show(
            "acme/api#1".into(),
            2,
            "alice requested changes".into(),
            true
        )]
    );
}

#[test]
fn opening_from_the_tui_also_reminds_after_the_interval() {
    let mut h = harness().bootstrapped();
    h.poll(vec![pr_with_changes(&["r1"])]);
    h.notifier.take();
    h.daemon
        .store
        .db()
        .enqueue(
            &Command::Ack {
                pr_key: "acme/api#1".into(),
            },
            t(0),
        )
        .unwrap();
    h.daemon.consume_commands().unwrap();
    assert_eq!(h.notifier.take(), vec![Call::Remove("acme/api#1".into())]);
    h.clock.advance(Duration::minutes(30));
    h.daemon.tick().unwrap();
    assert_eq!(h.notifier.take().len(), 1);
}

#[test]
fn a_pr_tackled_after_opening_gets_no_reminder() {
    let mut h = harness().bootstrapped();
    h.poll(vec![pr_with_changes(&["r1"])]);
    h.notifier.take();
    h.daemon
        .handle(Delivered {
            pr_key: "acme/api#1".into(),
            generation: 1,
            response: Response::Opened,
        })
        .unwrap();
    h.clock.advance(Duration::minutes(10));
    h.poll(vec![]);
    h.daemon.tick().unwrap();
    h.clock.advance(Duration::minutes(30));
    h.daemon.tick().unwrap();
    assert!(h.notifier.take().iter().all(|call| matches!(call, Call::Remove(_))));
}

#[test]
fn snooze_from_notification_resumes_after_duration() {
    let mut h = harness().bootstrapped();
    h.poll(vec![pr_with_changes(&["r1"])]);
    h.notifier.take();
    h.daemon
        .handle(Delivered {
            pr_key: "acme/api#1".into(),
            generation: 1,
            response: Response::Snoozed("15m".into()),
        })
        .unwrap();
    h.clock.advance(Duration::minutes(14));
    h.daemon.tick().unwrap();
    assert!(h.notifier.take().is_empty());
    h.clock.advance(Duration::minutes(1));
    h.daemon.tick().unwrap();
    assert_eq!(h.notifier.take().len(), 1);
}

#[test]
fn tackling_the_pr_removes_the_notification() {
    let mut h = harness().bootstrapped();
    h.poll(vec![pr_with_changes(&["r1"])]);
    h.notifier.take();
    let mut fixed = pr_with_changes(&["r1"]);
    fixed.head_oid = "new-head".into();
    h.poll(vec![fixed]);
    h.daemon.tick().unwrap();
    assert_eq!(h.notifier.take(), vec![Call::Remove("acme/api#1".into())]);
    assert!(h.daemon.store.db().prs(Tab::Mine).unwrap().is_empty());
}

#[test]
fn merged_pr_disappearing_ends_the_cycle() {
    let mut h = harness().bootstrapped();
    h.poll(vec![pr_with_changes(&["r1"])]);
    h.notifier.take();
    h.poll(vec![]);
    h.daemon.tick().unwrap();
    assert_eq!(h.notifier.take(), vec![Call::Remove("acme/api#1".into())]);
}

#[test]
fn failed_poll_keeps_rows_and_records_error_with_backoff() {
    let mut h = harness().bootstrapped();
    h.poll(vec![pr_with_changes(&["r1"])]);
    h.github.push(Err(FetchError::Http("timeout".into())));
    h.daemon.poll_account("me-work").unwrap();
    let db = h.daemon.store.db();
    assert_eq!(db.prs(Tab::Mine).unwrap().len(), 1);
    assert_eq!(
        db.meta("last_error:me-work").unwrap().as_deref(),
        Some("http error: timeout")
    );
    assert_eq!(h.daemon.next_poll["me-work"], t(1));
}

#[test]
fn tui_commands_are_applied() {
    let mut h = harness().bootstrapped();
    h.poll(vec![pr_with_changes(&["r1"])]);
    h.notifier.take();
    h.daemon
        .store
        .db()
        .enqueue(
            &Command::Done {
                pr_key: "acme/api#1".into(),
            },
            t(0),
        )
        .unwrap();
    h.daemon.consume_commands().unwrap();
    assert_eq!(h.notifier.take(), vec![Call::Remove("acme/api#1".into())]);
    assert!(h.daemon.store.db().alert("acme/api#1").unwrap().done_at.is_some());
}

#[test]
fn refresh_command_makes_every_account_due() {
    let mut h = harness().bootstrapped();
    h.daemon.store.db().enqueue(&Command::Refresh, t(0)).unwrap();
    h.github.push(Ok(snapshot(vec![])));
    h.daemon.cycle().unwrap();
    assert!(h.github.0.borrow().is_empty());
}

#[test]
fn withheld_results_keep_missing_prs_for_a_grace_period() {
    let mut h = harness().bootstrapped();
    let review_request = crate::domain::model::ReviewRequest {
        base: base("acme/web", 5),
        direct: true,
        team: None,
        event: None,
        reviews: vec![],
    };
    h.github.push(Ok(AccountSnapshot {
        login: "me-work".into(),
        mine: vec![pr_with_changes(&["r1"]), pr_with_changes_on("acme/lib", 2, "r2")],
        to_review: vec![review_request],
        ..Default::default()
    }));
    h.daemon.poll_account("me-work").unwrap();
    h.notifier.take();
    let mut tackled = pr_with_changes_on("acme/lib", 2, "r2");
    tackled.head_oid = "new-head".into();
    let withheld = || AccountSnapshot {
        login: "me-work".into(),
        errors: vec!["SAML SSO required".into()],
        withheld_mine: true,
        withheld_review: true,
        ..Default::default()
    };
    h.github.push(Ok(AccountSnapshot {
        mine: vec![tackled],
        ..withheld()
    }));
    h.daemon.poll_account("me-work").unwrap();
    h.daemon.tick().unwrap();
    assert_eq!(h.notifier.take(), vec![Call::Remove("acme/lib#2".into())]);
    let db = h.daemon.store.db();
    let mine: Vec<String> = db.prs(Tab::Mine).unwrap().into_iter().map(|r| r.key).collect();
    assert_eq!(mine, vec!["acme/api#1"]);
    assert_eq!(db.prs(Tab::Review).unwrap().len(), 1);
    assert_eq!(
        db.meta("last_error:me-work").unwrap().as_deref(),
        Some("SAML SSO required")
    );
    h.clock.advance(Duration::minutes(30));
    h.github.push(Ok(withheld()));
    h.daemon.poll_account("me-work").unwrap();
    assert_eq!(h.daemon.store.db().prs(Tab::Mine).unwrap().len(), 1);
    h.clock.advance(Duration::minutes(31));
    h.github.push(Ok(withheld()));
    h.daemon.poll_account("me-work").unwrap();
    assert!(h.daemon.store.db().prs(Tab::Mine).unwrap().is_empty());
}

#[test]
fn withheld_rows_are_kept_per_tab_and_until_exactly_the_grace_period() {
    let mut h = harness().bootstrapped();
    let review_request = crate::domain::model::ReviewRequest {
        base: base("acme/web", 5),
        direct: true,
        team: None,
        event: None,
        reviews: vec![],
    };
    h.github.push(Ok(AccountSnapshot {
        login: "me-work".into(),
        mine: vec![pr_with_changes(&["r1"])],
        to_review: vec![review_request],
        ..Default::default()
    }));
    h.daemon.poll_account("me-work").unwrap();
    let mine_withheld = || AccountSnapshot {
        login: "me-work".into(),
        withheld_mine: true,
        ..Default::default()
    };
    h.github.push(Ok(mine_withheld()));
    h.daemon.poll_account("me-work").unwrap();
    assert_eq!(h.daemon.store.db().prs(Tab::Mine).unwrap().len(), 1);
    assert!(h.daemon.store.db().prs(Tab::Review).unwrap().is_empty());
    h.clock.advance(Duration::minutes(59));
    h.github.push(Ok(mine_withheld()));
    h.daemon.poll_account("me-work").unwrap();
    assert_eq!(h.daemon.store.db().prs(Tab::Mine).unwrap().len(), 1);
    h.clock.advance(Duration::minutes(1));
    h.github.push(Ok(mine_withheld()));
    h.daemon.poll_account("me-work").unwrap();
    assert!(h.daemon.store.db().prs(Tab::Mine).unwrap().is_empty());
}

#[test]
fn withheld_rows_never_borrow_another_accounts_rows() {
    let mut h = harness_for(Store::open_in_memory().unwrap(), &["me-work", "me-home"]);
    for login in ["me-work", "me-home"] {
        h.github.push(Ok(AccountSnapshot {
            login: login.into(),
            ..Default::default()
        }));
        h.daemon.poll_account(login).unwrap();
    }
    h.daemon.tick().unwrap();
    h.github.push(Ok(AccountSnapshot {
        login: "me-work".into(),
        mine: vec![pr_with_changes(&["r1"])],
        ..Default::default()
    }));
    h.daemon.poll_account("me-work").unwrap();
    let mut home_pr = my_pr("oss/lib", 9);
    home_pr.reviews = vec![review(
        "h1",
        Author::user("bob"),
        ReviewState::ChangesRequested,
        0,
        "head",
    )];
    h.github.push(Ok(AccountSnapshot {
        login: "me-home".into(),
        mine: vec![home_pr],
        ..Default::default()
    }));
    h.daemon.poll_account("me-home").unwrap();
    h.github.push(Ok(AccountSnapshot {
        login: "me-work".into(),
        withheld_mine: true,
        ..Default::default()
    }));
    h.daemon.poll_account("me-work").unwrap();
    let rows = h.daemon.store.db().prs(Tab::Mine).unwrap();
    let owners: Vec<(&str, &str)> = rows.iter().map(|r| (r.account.as_str(), r.key.as_str())).collect();
    assert_eq!(owners, vec![("me-work", "acme/api#1"), ("me-home", "oss/lib#9")]);
    assert!(h.daemon.store.db().meta("last_error:me-work").unwrap().is_none());
}

#[test]
fn errors_without_withheld_results_do_not_keep_missing_prs() {
    let mut h = harness().bootstrapped();
    h.poll(vec![pr_with_changes(&["r1"])]);
    h.github.push(Ok(AccountSnapshot {
        login: "me-work".into(),
        errors: vec!["something else".into()],
        ..Default::default()
    }));
    h.daemon.poll_account("me-work").unwrap();
    assert!(h.daemon.store.db().prs(Tab::Mine).unwrap().is_empty());
}

#[test]
fn history_of_a_pr_that_appears_later_is_not_notified() {
    let mut h = harness().bootstrapped();
    h.clock.advance(Duration::minutes(60));
    h.poll(vec![]);
    h.clock.advance(Duration::minutes(5));
    let mut old = my_pr("acme/old", 2);
    old.reviews = vec![review(
        "r-old",
        Author::user("alice"),
        ReviewState::ChangesRequested,
        49,
        "head",
    )];
    let mut edge = my_pr("acme/edge", 3);
    edge.reviews = vec![review(
        "r-edge",
        Author::user("bob"),
        ReviewState::ChangesRequested,
        50,
        "head",
    )];
    h.poll(vec![old, edge]);
    assert_eq!(
        h.notifier.take(),
        vec![Call::Show(
            "acme/edge#3".into(),
            1,
            "bob requested changes".into(),
            true
        )]
    );
}

#[test]
fn activity_on_a_known_pr_is_never_treated_as_history() {
    let mut h = harness().bootstrapped();
    h.poll(vec![pr_with_changes(&["r1"])]);
    h.notifier.take();
    h.clock.advance(Duration::minutes(30));
    h.github.push(Ok(AccountSnapshot {
        login: "me-work".into(),
        withheld_mine: true,
        ..Default::default()
    }));
    h.daemon.poll_account("me-work").unwrap();
    h.clock.advance(Duration::minutes(5));
    let mut pr = pr_with_changes(&["r1"]);
    pr.reviews.push(review(
        "r2",
        Author::user("bob"),
        ReviewState::ChangesRequested,
        5,
        "head",
    ));
    h.poll(vec![pr]);
    let shown = h.notifier.take();
    assert_eq!(shown.len(), 1);
    assert!(matches!(&shown[0], Call::Show(key, _, message, true) if key == "acme/api#1" && message.contains("bob")));
}

#[test]
fn first_review_on_a_new_pr_withheld_for_a_while_still_notifies() {
    let mut h = harness().bootstrapped();
    h.poll(vec![my_pr("acme/api", 1)]);
    for minutes in [5, 25] {
        h.clock.advance(Duration::minutes(minutes));
        h.github.push(Ok(AccountSnapshot {
            login: "me-work".into(),
            withheld_mine: true,
            ..Default::default()
        }));
        h.daemon.poll_account("me-work").unwrap();
    }
    h.clock.advance(Duration::minutes(1));
    let mut pr = my_pr("acme/api", 1);
    pr.reviews = vec![review(
        "r1",
        Author::user("alice"),
        ReviewState::ChangesRequested,
        8,
        "head",
    )];
    h.poll(vec![pr]);
    assert_eq!(
        h.notifier.take(),
        vec![Call::Show(
            "acme/api#1".into(),
            1,
            "alice requested changes".into(),
            true
        )]
    );
}

#[test]
fn restarting_the_daemon_reshows_pinging_alerts_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let mut h = harness_with(Store::open(&path).unwrap()).bootstrapped();
    h.poll(vec![pr_with_changes(&["r1"])]);
    h.notifier.take();
    let mut restarted = harness_with(Store::open(&path).unwrap());
    restarted.clock.advance(Duration::minutes(1));
    restarted.daemon.tick().unwrap();
    assert_eq!(
        restarted.notifier.take(),
        vec![Call::Show(
            "acme/api#1".into(),
            2,
            "alice requested changes".into(),
            true
        )]
    );
    restarted.daemon.tick().unwrap();
    assert!(restarted.notifier.take().is_empty());
}

#[test]
fn a_notifier_reset_reshows_pinging_alerts_at_once() {
    let mut h = harness().bootstrapped();
    h.poll(vec![pr_with_changes(&["r1"])]);
    h.notifier.take();
    h.clock.advance(Duration::minutes(1));
    h.daemon.tick().unwrap();
    assert!(h.notifier.take().is_empty());
    h.notifier.reset.set(true);
    h.daemon.tick().unwrap();
    assert_eq!(
        h.notifier.take(),
        vec![Call::Show(
            "acme/api#1".into(),
            2,
            "alice requested changes".into(),
            true
        )]
    );
    h.daemon.tick().unwrap();
    assert!(h.notifier.take().is_empty());
}

#[test]
fn a_notifier_restarted_by_a_show_reshows_the_others_but_not_that_one() {
    let mut h = harness().bootstrapped();
    h.poll(vec![
        pr_with_changes_on("acme/api", 1, "r1"),
        pr_with_changes_on("acme/web", 2, "r2"),
    ]);
    h.notifier.take();
    h.clock.advance(Duration::minutes(1));
    h.notifier.reset_on_show.set(true);
    let mut api = pr_with_changes_on("acme/api", 1, "r1");
    api.reviews.push(review(
        "r3",
        Author::user("bob"),
        ReviewState::ChangesRequested,
        1,
        "head",
    ));
    h.poll(vec![api, pr_with_changes_on("acme/web", 2, "r2")]);
    h.daemon.tick().unwrap();
    let shown: Vec<String> = h
        .notifier
        .take()
        .into_iter()
        .map(|call| match call {
            Call::Show(key, ..) => key,
            Call::Remove(key) => format!("remove {key}"),
        })
        .collect();
    assert_eq!(shown, vec!["acme/api#1".to_string(), "acme/web#2".to_string()]);
}

#[test]
fn a_notifier_reset_leaves_snoozed_and_opened_alerts_alone() {
    let mut h = harness().bootstrapped();
    h.poll(vec![
        pr_with_changes_on("acme/api", 1, "r1"),
        pr_with_changes_on("acme/web", 2, "r2"),
        pr_with_changes_on("acme/cli", 3, "r3"),
    ]);
    h.notifier.take();
    for (key, response) in [
        ("acme/api#1", Response::Snoozed("1h".into())),
        ("acme/web#2", Response::Opened),
    ] {
        h.daemon
            .handle(Delivered {
                pr_key: key.into(),
                generation: 1,
                response,
            })
            .unwrap();
    }
    h.notifier.take();
    h.clock.advance(Duration::minutes(1));
    h.notifier.reset.set(true);
    h.daemon.tick().unwrap();
    let shown: Vec<String> = h
        .notifier
        .take()
        .into_iter()
        .filter_map(|call| match call {
            Call::Show(key, ..) => Some(key),
            Call::Remove(_) => None,
        })
        .collect();
    assert_eq!(shown, vec!["acme/cli#3".to_string()]);
}

#[test]
fn resync_skips_prs_already_shown_this_cycle() {
    let mut h = harness().bootstrapped();
    h.daemon.resync = true;
    h.poll(vec![pr_with_changes(&["r1"])]);
    h.daemon.tick().unwrap();
    assert_eq!(h.notifier.take().len(), 1);
}

#[test]
fn low_rate_limit_delays_the_next_poll_until_reset() {
    let mut h = harness().bootstrapped();
    h.github.push(Ok(AccountSnapshot {
        login: "me-work".into(),
        rate_remaining: Some(50),
        rate_reset_at: Some(t(40)),
        ..Default::default()
    }));
    h.daemon.poll_account("me-work").unwrap();
    assert_eq!(h.daemon.next_poll["me-work"], t(40));
}

#[test]
fn rows_of_accounts_removed_from_the_config_are_pruned() {
    let mut h = harness().bootstrapped();
    let stale = PrRow::review(
        "someone-else",
        &crate::domain::model::ReviewRequest {
            base: base("oss/lib", 9),
            direct: true,
            team: None,
            event: None,
            reviews: vec![],
        },
    );
    h.daemon
        .store
        .db()
        .replace_account_prs("someone-else", &[stale])
        .unwrap();
    h.daemon.store.db().set_meta("bootstrapped:someone-else", "x").unwrap();
    h.daemon.prune_unknown_accounts().unwrap();
    assert!(h.daemon.store.db().prs(Tab::Review).unwrap().is_empty());
    assert!(h.daemon.store.db().meta("bootstrapped:someone-else").unwrap().is_none());
    assert!(h.daemon.store.db().meta("bootstrapped:me-work").unwrap().is_some());
}

#[test]
fn idle_alerts_of_gone_prs_are_dropped_after_a_day() {
    let mut h = harness().bootstrapped();
    let mut approved = my_pr("acme/api", 1);
    approved.reviews = vec![review("r1", Author::user("alice"), ReviewState::Approved, 0, "head")];
    h.poll(vec![approved]);
    h.clock.advance(Duration::hours(2));
    h.daemon.tick().unwrap();
    assert_eq!(h.daemon.store.db().alerts().unwrap().len(), 1);
    h.clock.advance(Duration::hours(23));
    h.daemon.tick().unwrap();
    assert!(h.daemon.store.db().alerts().unwrap().is_empty());
}

#[test]
fn truncation_warnings_are_logged_only_when_they_change() {
    let a = "acme/api#1: more than 50 reviews, older ones ignored".to_string();
    let b = "acme/web#2: more than 50 reviews, older ones ignored".to_string();
    assert_eq!(new_warnings(&[], std::slice::from_ref(&a)), vec![&a]);
    assert!(new_warnings(std::slice::from_ref(&a), std::slice::from_ref(&a)).is_empty());
    assert_eq!(
        new_warnings(std::slice::from_ref(&a), &[a.clone(), b.clone()]),
        vec![&b]
    );
}

#[test]
fn backoff_doubles_up_to_five_minutes() {
    let base = Duration::seconds(60);
    assert_eq!(backoff(base, 1), Duration::seconds(60));
    assert_eq!(backoff(base, 2), Duration::seconds(120));
    assert_eq!(backoff(base, 3), Duration::seconds(240));
    assert_eq!(backoff(base, 4), Duration::seconds(300));
    assert_eq!(backoff(base, 20), Duration::seconds(300));
}

#[test]
fn review_requests_waiting_at_the_first_poll_stay_silent() {
    let mut h = harness();
    h.poll_requests(vec![request("acme/web", 7)]);
    h.daemon.tick().unwrap();
    h.clock.advance(Duration::minutes(10));
    h.poll_requests(vec![request("acme/web", 7)]);
    h.daemon.tick().unwrap();
    assert!(h.notifier.take().is_empty());
    h.poll_requests(vec![request("acme/web", 7), request("acme/web", 8)]);
    assert_eq!(
        shown_keys(h.notifier.take()),
        vec![("acme/web#8".into(), 1, "dave requested your review".into())]
    );
}

#[test]
fn a_review_request_pings_until_it_leaves_the_list() {
    let mut h = harness().bootstrapped();
    h.poll_requests(vec![request("acme/web", 7)]);
    assert_eq!(
        h.notifier.take(),
        vec![Call::Show(
            "acme/web#7".into(),
            1,
            "dave requested your review".into(),
            true
        )]
    );
    h.clock.advance(Duration::minutes(5));
    h.daemon.tick().unwrap();
    assert_eq!(
        shown_keys(h.notifier.take()),
        vec![("acme/web#7".into(), 2, "dave requested your review".into())]
    );
    h.poll_requests(vec![]);
    h.daemon.tick().unwrap();
    assert_eq!(h.notifier.take(), vec![Call::Remove("acme/web#7".into())]);
    h.clock.advance(Duration::minutes(30));
    h.daemon.tick().unwrap();
    assert!(h.notifier.take().is_empty());
}

#[test]
fn a_re_requested_review_notifies_again_but_a_search_flap_does_not() {
    let mut h = harness().bootstrapped();
    h.poll_requests(vec![request("acme/web", 7)]);
    h.poll_requests(vec![]);
    h.daemon.tick().unwrap();
    h.notifier.take();
    h.clock.advance(Duration::minutes(1));
    h.poll_requests(vec![request("acme/web", 7)]);
    h.daemon.tick().unwrap();
    assert!(h.notifier.take().is_empty());
    h.poll_requests(vec![re_requested("acme/web", 7, "erin")]);
    assert_eq!(
        shown_keys(h.notifier.take()),
        vec![("acme/web#7".into(), 2, "erin requested your review".into())]
    );
}

#[test]
fn opening_a_requested_review_pauses_it_until_the_reminder() {
    let mut h = harness().bootstrapped();
    h.poll_requests(vec![request("acme/web", 7)]);
    h.notifier.take();
    h.daemon
        .handle(Delivered {
            pr_key: "acme/web#7".into(),
            generation: 1,
            response: Response::Opened,
        })
        .unwrap();
    assert_eq!(
        *h.opened.borrow(),
        vec!["me-work https://github.com/acme/web/pull/7".to_string()]
    );
    h.clock.advance(Duration::minutes(29));
    h.daemon.tick().unwrap();
    assert!(h.notifier.take().is_empty());
    h.clock.advance(Duration::minutes(1));
    h.daemon.tick().unwrap();
    assert_eq!(shown_keys(h.notifier.take()).len(), 1);
}

#[test]
fn team_requests_ping_only_when_enabled_and_enabling_does_not_flood() {
    let mut h = harness().bootstrapped();
    h.poll_requests(vec![team_request("acme/web", 7)]);
    assert!(h.notifier.take().is_empty());
    h.daemon.config.notify_team_requests = true;
    h.poll_requests(vec![team_request("acme/web", 7)]);
    assert!(h.notifier.take().is_empty());
    h.poll_requests(vec![team_request("acme/web", 7), team_request("acme/web", 8)]);
    assert_eq!(
        shown_keys(h.notifier.take()),
        vec![("acme/web#8".into(), 1, "dave requested a review from your team".into())]
    );
}

#[test]
fn drafts_and_bot_pull_requests_never_ping() {
    let mut h = harness().bootstrapped();
    let mut draft = request("acme/web", 7);
    draft.base.is_draft = true;
    let mut bot = request("acme/web", 8);
    bot.base.author = "dependabot".into();
    bot.base.author_is_bot = true;
    h.poll_requests(vec![draft, bot]);
    assert!(h.notifier.take().is_empty());
    let mut ready = request("acme/web", 7);
    ready.base.is_draft = false;
    h.poll_requests(vec![ready]);
    assert_eq!(shown_keys(h.notifier.take()).len(), 1);
}

#[test]
fn pings_stop_when_the_pr_goes_back_to_draft_or_only_a_team_request_remains() {
    let mut h = harness().bootstrapped();
    h.poll_requests(vec![request("acme/web", 7), request("acme/web", 8)]);
    h.notifier.take();
    let mut draft = request("acme/web", 7);
    draft.base.is_draft = true;
    let team_only = team_request("acme/web", 8);
    h.clock.advance(Duration::minutes(5));
    h.poll_requests(vec![draft, team_only]);
    h.daemon.tick().unwrap();
    let mut calls = h.notifier.take();
    calls.sort_by_key(|c| format!("{c:?}"));
    assert_eq!(
        calls,
        vec![Call::Remove("acme/web#7".into()), Call::Remove("acme/web#8".into())]
    );
}

#[test]
fn a_dismissed_review_request_stays_quiet_until_asked_again() {
    let mut h = harness().bootstrapped();
    h.poll_requests(vec![request("acme/web", 7)]);
    h.notifier.take();
    h.daemon
        .store
        .db()
        .enqueue(
            &Command::Done {
                pr_key: "acme/web#7".into(),
            },
            t(0),
        )
        .unwrap();
    h.daemon.consume_commands().unwrap();
    assert_eq!(h.notifier.take(), vec![Call::Remove("acme/web#7".into())]);
    h.clock.advance(Duration::minutes(10));
    h.poll_requests(vec![request("acme/web", 7)]);
    h.daemon.tick().unwrap();
    assert!(h.notifier.take().is_empty());
    assert!(h.daemon.store.db().alert("acme/web#7").unwrap().done_at.is_some());
    h.poll_requests(vec![re_requested("acme/web", 7, "dave")]);
    assert_eq!(shown_keys(h.notifier.take()).len(), 1);
}

#[test]
fn a_request_that_goes_to_draft_and_back_pings_again_unless_dismissed() {
    let mut h = harness().bootstrapped();
    h.poll_requests(vec![request("acme/web", 7), request("acme/web", 8)]);
    h.notifier.take();
    h.daemon
        .store
        .db()
        .enqueue(
            &Command::Done {
                pr_key: "acme/web#8".into(),
            },
            t(0),
        )
        .unwrap();
    h.daemon.consume_commands().unwrap();
    let as_draft = |number| {
        let mut r = request("acme/web", number);
        r.base.is_draft = true;
        r
    };
    h.poll_requests(vec![as_draft(7), as_draft(8)]);
    h.daemon.tick().unwrap();
    h.notifier.take();
    h.clock.advance(Duration::minutes(10));
    h.poll_requests(vec![request("acme/web", 7), request("acme/web", 8)]);
    assert_eq!(
        shown_keys(h.notifier.take()),
        vec![("acme/web#7".into(), 2, "dave requested your review".into())]
    );
    h.poll_requests(vec![request("acme/web", 7), request("acme/web", 8)]);
    assert!(h.notifier.take().is_empty());
}

#[test]
fn a_team_request_on_another_account_does_not_end_a_direct_one() {
    let mut h = harness_for(Store::open_in_memory().unwrap(), &["me-work", "me-home"]).bootstrapped();
    h.github.push(Ok(AccountSnapshot {
        login: "me-home".into(),
        ..Default::default()
    }));
    h.daemon.poll_account("me-home").unwrap();
    h.poll_requests(vec![request("acme/web", 7)]);
    h.github.push(Ok(AccountSnapshot {
        login: "me-home".into(),
        to_review: vec![team_request("acme/web", 7)],
        ..Default::default()
    }));
    h.daemon.poll_account("me-home").unwrap();
    h.notifier.take();
    h.clock.advance(Duration::minutes(5));
    h.daemon.tick().unwrap();
    assert_eq!(shown_keys(h.notifier.take()).len(), 1);
}

#[test]
fn a_request_whose_event_is_out_of_reach_does_not_ping_twice() {
    let mut h = harness().bootstrapped();
    h.poll_requests(vec![request("acme/web", 7)]);
    h.notifier.take();
    let mut no_event = request("acme/web", 7);
    no_event.event = None;
    h.poll_requests(vec![no_event.clone()]);
    assert!(h.notifier.take().is_empty());
    let mut fresh = request("acme/web", 9);
    fresh.event = None;
    h.poll_requests(vec![no_event, fresh]);
    assert_eq!(
        shown_keys(h.notifier.take()),
        vec![("acme/web#9".into(), 1, "dave requested your review".into())]
    );
}

fn reviewed_by_raad(mut r: ReviewRequest) -> ReviewRequest {
    r.reviews = vec![ReviewMark {
        author: Author::user("raad"),
        state: ReviewState::ChangesRequested,
        submitted_at: Some(t(0)),
    }];
    r
}

#[test]
fn a_request_someone_else_already_reviewed_does_not_ping() {
    let mut h = harness().bootstrapped();
    h.poll_requests(vec![reviewed_by_raad(request("acme/web", 7))]);
    assert!(h.notifier.take().is_empty());
    let row = h.daemon.store.db().pr(Tab::Review, "acme/web#7").unwrap().unwrap();
    assert_eq!(
        row.reasons,
        vec![Reason::ChangesRequested {
            by: vec!["raad".into()]
        }]
    );
}

#[test]
fn pings_stop_once_someone_else_reviews() {
    let mut h = harness().bootstrapped();
    h.poll_requests(vec![request("acme/web", 7)]);
    h.notifier.take();
    h.clock.advance(Duration::minutes(2));
    h.poll_requests(vec![reviewed_by_raad(request("acme/web", 7))]);
    h.daemon.tick().unwrap();
    assert_eq!(h.notifier.take(), vec![Call::Remove("acme/web#7".into())]);
    h.clock.advance(Duration::minutes(10));
    h.daemon.tick().unwrap();
    assert!(h.notifier.take().is_empty());
}
