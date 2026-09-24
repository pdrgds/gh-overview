use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::activity::{ActivityItem, summarize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AlertState {
    pub cycle_started_at: Option<DateTime<Utc>>,
    pub pending: Vec<ActivityItem>,
    pub last_notified_at: Option<DateTime<Utc>>,
    pub snoozed_until: Option<DateTime<Utc>>,
    pub acked_at: Option<DateTime<Utc>>,
    pub done_at: Option<DateTime<Utc>>,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Pinging,
    Snoozed,
    Acked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    NewActivity { items: Vec<ActivityItem>, untackled: bool },
    Tick { untackled: bool },
    Opened { generation: u64 },
    SnoozedFromNotification { generation: u64, until: DateTime<Utc> },
    Ack,
    Snooze { until: DateTime<Utc> },
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Notify {
        generation: u64,
        summary: String,
        snoozable: bool,
    },
    Remove,
    OpenUrl,
}

impl AlertState {
    pub fn phase(&self, now: DateTime<Utc>) -> Phase {
        if self.cycle_started_at.is_none() {
            Phase::Idle
        } else if let Some(until) = self.snoozed_until {
            if until > now { Phase::Snoozed } else { Phase::Pinging }
        } else if self.acked_at.is_some() {
            Phase::Acked
        } else {
            Phase::Pinging
        }
    }
}

pub fn step(state: &AlertState, event: Event, now: DateTime<Utc>, renotify: Duration) -> (AlertState, Vec<Effect>) {
    let mut s = state.clone();
    let mut fx = Vec::new();
    let phase = state.phase(now);
    let active = phase != Phase::Idle;
    match event {
        Event::NewActivity { items, untackled: true } => {
            if matches!(phase, Phase::Idle | Phase::Acked) {
                s.pending.clear();
            }
            s.pending.extend(items);
            s.cycle_started_at.get_or_insert(now);
            s.acked_at = None;
            s.snoozed_until = None;
            s.done_at = None;
            let summary = summarize(&s.pending);
            notify(&mut s, &mut fx, now, summary, true);
        }
        Event::NewActivity {
            items,
            untackled: false,
        } => {
            end_cycle(&mut s);
            s.done_at = None;
            notify(&mut s, &mut fx, now, summarize(&items), false);
        }
        Event::Tick { untackled } => match phase {
            Phase::Idle => {}
            _ if !untackled => {
                end_cycle(&mut s);
                fx.push(Effect::Remove);
            }
            Phase::Snoozed | Phase::Acked => {}
            Phase::Pinging => {
                let snooze_expired = s.snoozed_until.is_some();
                let due = s.last_notified_at.is_none_or(|t| now < t || now - t >= renotify);
                if snooze_expired || due {
                    s.snoozed_until = None;
                    s.acked_at = None;
                    let summary = summarize(&s.pending);
                    notify(&mut s, &mut fx, now, summary, true);
                }
            }
        },
        Event::Opened { generation } => {
            fx.push(Effect::OpenUrl);
            if generation == state.generation && active {
                s.acked_at = Some(now);
                s.snoozed_until = None;
            }
        }
        Event::SnoozedFromNotification { generation, until } => {
            if generation == state.generation && active {
                s.snoozed_until = Some(until);
            }
        }
        Event::Ack => {
            if active {
                s.acked_at = Some(now);
                s.snoozed_until = None;
                fx.push(Effect::Remove);
            }
        }
        Event::Snooze { until } => {
            if active {
                s.snoozed_until = Some(until);
                fx.push(Effect::Remove);
            }
        }
        Event::Done => {
            end_cycle(&mut s);
            s.done_at = Some(now);
            fx.push(Effect::Remove);
        }
    }
    (s, fx)
}

fn notify(s: &mut AlertState, fx: &mut Vec<Effect>, now: DateTime<Utc>, summary: String, snoozable: bool) {
    s.generation += 1;
    s.last_notified_at = Some(now);
    fx.push(Effect::Notify {
        generation: s.generation,
        summary,
        snoozable,
    });
}

fn end_cycle(s: &mut AlertState) {
    s.cycle_started_at = None;
    s.pending.clear();
    s.acked_at = None;
    s.snoozed_until = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::activity::ActivityKind;
    use crate::domain::model::ReviewState;
    use crate::domain::testkit::t;

    const RENOTIFY: Duration = Duration::minutes(5);

    fn item(id: &str, actor: &str, state: Option<ReviewState>) -> ActivityItem {
        ActivityItem {
            id: id.to_string(),
            kind: if state.is_some() {
                ActivityKind::Review
            } else {
                ActivityKind::Comment
            },
            actor: actor.to_string(),
            review_state: state,
            comment_count: 1,
            at: t(0),
        }
    }

    fn changes(actor: &str) -> Event {
        Event::NewActivity {
            items: vec![item("r", actor, Some(ReviewState::ChangesRequested))],
            untackled: true,
        }
    }

    fn notified(fx: &[Effect]) -> Vec<(u64, String)> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Notify {
                    generation, summary, ..
                } => Some((*generation, summary.clone())),
                _ => None,
            })
            .collect()
    }

    fn pinging() -> AlertState {
        step(&AlertState::default(), changes("alice"), t(0), RENOTIFY).0
    }

    #[test]
    fn new_actionable_activity_notifies_and_starts_pinging() {
        let (s, fx) = step(&AlertState::default(), changes("alice"), t(0), RENOTIFY);
        assert_eq!(notified(&fx), vec![(1, "alice requested changes".to_string())]);
        assert_eq!(s.phase(t(0)), Phase::Pinging);
    }

    #[test]
    fn renotifies_only_after_interval() {
        let s = pinging();
        let (s, fx) = step(&s, Event::Tick { untackled: true }, t(4), RENOTIFY);
        assert!(fx.is_empty());
        let (s, fx) = step(&s, Event::Tick { untackled: true }, t(5), RENOTIFY);
        assert_eq!(notified(&fx), vec![(2, "alice requested changes".to_string())]);
        assert_eq!(s.last_notified_at, Some(t(5)));
    }

    #[test]
    fn sleep_catch_up_fires_once() {
        let (s, fx) = step(&pinging(), Event::Tick { untackled: true }, t(120), RENOTIFY);
        assert_eq!(notified(&fx).len(), 1);
        let (_, fx) = step(&s, Event::Tick { untackled: true }, t(120), RENOTIFY);
        assert!(fx.is_empty());
    }

    #[test]
    fn summary_accumulates_while_pinging_and_resets_after_ack() {
        let (s, fx) = step(&pinging(), changes("bob"), t(1), RENOTIFY);
        assert_eq!(notified(&fx)[0].1, "alice requested changes · bob requested changes");
        let (s, _) = step(&s, Event::Ack, t(2), RENOTIFY);
        let (s, fx) = step(&s, changes("carol"), t(3), RENOTIFY);
        assert_eq!(notified(&fx)[0].1, "carol requested changes");
        assert_eq!(s.phase(t(3)), Phase::Pinging);
    }

    #[test]
    fn non_actionable_activity_notifies_once_without_a_cycle() {
        let approval = Event::NewActivity {
            items: vec![item("r", "bob", Some(ReviewState::Approved))],
            untackled: false,
        };
        let (s, fx) = step(&AlertState::default(), approval.clone(), t(0), RENOTIFY);
        assert_eq!(notified(&fx), vec![(1, "bob approved".to_string())]);
        assert_eq!(s.phase(t(0)), Phase::Idle);
        let (_, fx) = step(&s, Event::Tick { untackled: false }, t(30), RENOTIFY);
        assert!(fx.is_empty());

        let (s, fx) = step(&pinging(), approval, t(1), RENOTIFY);
        assert_eq!(s.phase(t(1)), Phase::Idle);
        assert!(!fx.contains(&Effect::Remove));
    }

    #[test]
    fn opened_current_generation_opens_and_acks() {
        let (s, fx) = step(&pinging(), Event::Opened { generation: 1 }, t(1), RENOTIFY);
        assert_eq!(fx, vec![Effect::OpenUrl]);
        assert_eq!(s.phase(t(1)), Phase::Acked);
        let (_, fx) = step(&s, Event::Tick { untackled: true }, t(30), RENOTIFY);
        assert!(fx.is_empty());
    }

    #[test]
    fn stale_generation_responses_open_the_pr_but_change_nothing() {
        let (s, _) = step(&pinging(), Event::Tick { untackled: true }, t(5), RENOTIFY);
        let (s2, fx) = step(&s, Event::Opened { generation: 1 }, t(6), RENOTIFY);
        assert_eq!(fx, vec![Effect::OpenUrl]);
        assert_eq!(s2, s);
        let (s3, _) = step(
            &s,
            Event::SnoozedFromNotification {
                generation: 1,
                until: t(60),
            },
            t(6),
            RENOTIFY,
        );
        assert_eq!(s3, s);
    }

    #[test]
    fn snooze_pauses_then_resumes_pinging() {
        let (s, fx) = step(
            &pinging(),
            Event::SnoozedFromNotification {
                generation: 1,
                until: t(15),
            },
            t(1),
            RENOTIFY,
        );
        assert!(fx.is_empty());
        assert_eq!(s.phase(t(10)), Phase::Snoozed);
        let (s, fx) = step(&s, Event::Tick { untackled: true }, t(10), RENOTIFY);
        assert!(fx.is_empty());
        let (s, fx) = step(&s, Event::Tick { untackled: true }, t(15), RENOTIFY);
        assert_eq!(notified(&fx).len(), 1);
        assert_eq!(s.phase(t(15)), Phase::Pinging);
    }

    #[test]
    fn snooze_after_ack_resumes_pinging_at_expiry() {
        let (s, _) = step(&pinging(), Event::Ack, t(1), RENOTIFY);
        let (s, fx) = step(&s, Event::Snooze { until: t(60) }, t(2), RENOTIFY);
        assert_eq!(fx, vec![Effect::Remove]);
        let (s, fx) = step(&s, Event::Tick { untackled: true }, t(60), RENOTIFY);
        assert_eq!(notified(&fx).len(), 1);
        assert_eq!(s.acked_at, None);
    }

    #[test]
    fn tackled_ends_cycle_and_removes_notification() {
        let (s, _) = step(&pinging(), Event::Ack, t(1), RENOTIFY);
        let (s, fx) = step(&s, Event::Tick { untackled: false }, t(2), RENOTIFY);
        assert_eq!(fx, vec![Effect::Remove]);
        assert_eq!(s.phase(t(2)), Phase::Idle);
        assert!(s.pending.is_empty());
    }

    #[test]
    fn ack_and_snooze_are_noops_when_idle() {
        let idle = AlertState::default();
        let (s, fx) = step(&idle, Event::Ack, t(0), RENOTIFY);
        assert!(fx.is_empty());
        assert_eq!(s, idle);
        let (s, fx) = step(&idle, Event::Snooze { until: t(10) }, t(0), RENOTIFY);
        assert!(fx.is_empty());
        assert_eq!(s, idle);
    }

    #[test]
    fn done_hides_until_new_activity() {
        let (s, fx) = step(&pinging(), Event::Done, t(1), RENOTIFY);
        assert_eq!(fx, vec![Effect::Remove]);
        assert_eq!(s.done_at, Some(t(1)));
        assert_eq!(s.phase(t(1)), Phase::Idle);
        let approval = Event::NewActivity {
            items: vec![item("r", "bob", Some(ReviewState::Approved))],
            untackled: false,
        };
        let (s, _) = step(&s, approval, t(2), RENOTIFY);
        assert_eq!(s.done_at, None);
    }

    #[test]
    fn only_actionable_notifications_offer_snooze() {
        let snoozable = |fx: &[Effect]| {
            fx.iter()
                .find_map(|e| match e {
                    Effect::Notify { snoozable, .. } => Some(*snoozable),
                    _ => None,
                })
                .unwrap()
        };
        let (s, fx) = step(&AlertState::default(), changes("alice"), t(0), RENOTIFY);
        assert!(snoozable(&fx));
        let (_, fx) = step(&s, Event::Tick { untackled: true }, t(5), RENOTIFY);
        assert!(snoozable(&fx));
        let approval = Event::NewActivity {
            items: vec![item("r", "bob", Some(ReviewState::Approved))],
            untackled: false,
        };
        let (_, fx) = step(&AlertState::default(), approval, t(0), RENOTIFY);
        assert!(!snoozable(&fx));
    }

    #[test]
    fn opening_a_one_shot_notification_opens_without_starting_a_cycle() {
        let approval = Event::NewActivity {
            items: vec![item("r", "bob", Some(ReviewState::Approved))],
            untackled: false,
        };
        let (s, _) = step(&AlertState::default(), approval, t(0), RENOTIFY);
        let (s2, fx) = step(&s, Event::Opened { generation: 1 }, t(1), RENOTIFY);
        assert_eq!(fx, vec![Effect::OpenUrl]);
        assert_eq!(s2, s);
    }

    #[test]
    fn tui_ack_removes_the_notification() {
        let (s, fx) = step(&pinging(), Event::Ack, t(1), RENOTIFY);
        assert_eq!(fx, vec![Effect::Remove]);
        assert_eq!(s.phase(t(1)), Phase::Acked);
    }

    #[test]
    fn ack_while_snoozed_stays_quiet_after_the_snooze() {
        let (s, _) = step(&pinging(), Event::Snooze { until: t(60) }, t(1), RENOTIFY);
        let (s, fx) = step(&s, Event::Ack, t(2), RENOTIFY);
        assert_eq!(fx, vec![Effect::Remove]);
        assert_eq!(s.phase(t(2)), Phase::Acked);
        let (_, fx) = step(&s, Event::Tick { untackled: true }, t(61), RENOTIFY);
        assert!(fx.is_empty());
    }

    #[test]
    fn new_activity_breaks_a_snooze_and_keeps_the_summary() {
        let (s, _) = step(
            &pinging(),
            Event::SnoozedFromNotification {
                generation: 1,
                until: t(60),
            },
            t(1),
            RENOTIFY,
        );
        let (s, fx) = step(&s, changes("bob"), t(2), RENOTIFY);
        assert_eq!(notified(&fx)[0].1, "alice requested changes · bob requested changes");
        assert_eq!(s.phase(t(2)), Phase::Pinging);
        assert_eq!(s.snoozed_until, None);
    }

    #[test]
    fn actionable_activity_clears_done() {
        let (s, _) = step(&pinging(), Event::Done, t(1), RENOTIFY);
        let (s, _) = step(&s, changes("bob"), t(2), RENOTIFY);
        assert_eq!(s.done_at, None);
        assert_eq!(s.phase(t(2)), Phase::Pinging);
    }

    #[test]
    fn snooze_expiry_fires_even_before_the_renotify_interval() {
        let renotify = Duration::minutes(10);
        let (s, _) = step(&AlertState::default(), changes("alice"), t(0), renotify);
        let (s, _) = step(
            &s,
            Event::SnoozedFromNotification {
                generation: 1,
                until: t(3),
            },
            t(1),
            renotify,
        );
        let (s, fx) = step(&s, Event::Tick { untackled: true }, t(3), renotify);
        assert_eq!(notified(&fx).len(), 1);
        let (_, fx) = step(&s, Event::Tick { untackled: true }, t(4), renotify);
        assert!(fx.is_empty());
    }

    #[test]
    fn opening_while_snoozed_acks_and_clears_the_snooze() {
        let (s, _) = step(&pinging(), Event::Snooze { until: t(60) }, t(1), RENOTIFY);
        let (s, fx) = step(&s, Event::Opened { generation: 1 }, t(2), RENOTIFY);
        assert_eq!(fx, vec![Effect::OpenUrl]);
        assert_eq!(s.snoozed_until, None);
        assert_eq!(s.phase(t(2)), Phase::Acked);
    }

    #[test]
    fn stored_state_tolerates_missing_fields() {
        let state: AlertState = serde_json::from_str("{}").unwrap();
        assert_eq!(state, AlertState::default());
    }

    #[test]
    fn clock_moving_backwards_does_not_silence_pings() {
        let (_, fx) = step(&pinging(), Event::Tick { untackled: true }, t(-50), RENOTIFY);
        assert_eq!(notified(&fx).len(), 1);
    }
}
