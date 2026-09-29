use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::actors::Identity;
use super::model::{MyPr, Review, ReviewMark, ReviewState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    Threads { count: u32 },
    ChangesRequested { by: Vec<String> },
    Comments { count: u32 },
    Approved { by: Vec<String> },
    Reviewed { by: Vec<String> },
}

pub fn reasons(pr: &MyPr, id: &Identity) -> Vec<Reason> {
    let mut out = Vec::new();
    let threads = unanswered_threads(pr, id);
    if threads > 0 {
        out.push(Reason::Threads { count: threads });
    }
    let by = changes_requested_on_head(pr, id);
    if !by.is_empty() {
        out.push(Reason::ChangesRequested { by });
    }
    let comments = unanswered_comments(pr, id);
    if comments > 0 {
        out.push(Reason::Comments { count: comments });
    }
    out
}

pub fn reviewed_by_others(reviews: &[ReviewMark], id: &Identity) -> Vec<Reason> {
    let mut ordered: Vec<&ReviewMark> = reviews.iter().filter(|r| r.submitted_at.is_some()).collect();
    ordered.sort_by_key(|r| r.submitted_at);
    let mut latest: BTreeMap<&str, ReviewState> = BTreeMap::new();
    for r in ordered {
        if id.is_me(&r.author.login) || id.is_bot(&r.author) {
            continue;
        }
        match r.state {
            ReviewState::Approved | ReviewState::ChangesRequested => {
                latest.insert(&r.author.login, r.state);
            }
            ReviewState::Commented => {
                latest.entry(&r.author.login).or_insert(r.state);
            }
            _ => {}
        }
    }
    let by = |state: ReviewState| -> Vec<String> {
        latest
            .iter()
            .filter(|(_, s)| **s == state)
            .map(|(login, _)| login.to_string())
            .collect()
    };
    let mut out = Vec::new();
    let changes = by(ReviewState::ChangesRequested);
    if !changes.is_empty() {
        out.push(Reason::ChangesRequested { by: changes });
    }
    let approved = by(ReviewState::Approved);
    if !approved.is_empty() {
        out.push(Reason::Approved { by: approved });
    }
    let commented = by(ReviewState::Commented);
    if !commented.is_empty() {
        out.push(Reason::Reviewed { by: commented });
    }
    out
}

pub fn describe(reasons: &[Reason]) -> String {
    reasons
        .iter()
        .map(|r| match r {
            Reason::Threads { count } => plural(*count, "thread"),
            Reason::ChangesRequested { by } => format!("changes: {}", by.join(", ")),
            Reason::Comments { count } => plural(*count, "comment"),
            Reason::Approved { by } => format!("approved: {}", by.join(", ")),
            Reason::Reviewed { by } => format!("reviewed: {}", by.join(", ")),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

pub fn plural(n: u32, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn unanswered_threads(pr: &MyPr, id: &Identity) -> u32 {
    pr.threads
        .iter()
        .filter(|t| {
            !t.is_resolved
                && t.first_author.as_deref().is_none_or(|a| !id.is_me(a))
                && t.last_author.as_deref().is_none_or(|a| !id.is_me(a))
        })
        .count() as u32
}

fn changes_requested_on_head(pr: &MyPr, id: &Identity) -> Vec<String> {
    let mut latest: BTreeMap<&str, &Review> = BTreeMap::new();
    for r in &pr.reviews {
        let decisive = matches!(
            r.state,
            ReviewState::Approved | ReviewState::ChangesRequested | ReviewState::Dismissed
        );
        if id.is_me(&r.author.login) || !decisive || r.submitted_at.is_none() {
            continue;
        }
        let newer = latest
            .get(r.author.login.as_str())
            .is_none_or(|prev| prev.submitted_at < r.submitted_at);
        if newer {
            latest.insert(r.author.login.as_str(), r);
        }
    }
    latest
        .into_iter()
        .filter(|(_, r)| {
            r.state == ReviewState::ChangesRequested && r.commit_oid.as_deref() == Some(pr.head_oid.as_str())
        })
        .map(|(login, _)| login.to_string())
        .collect()
}

fn unanswered_comments(pr: &MyPr, id: &Identity) -> u32 {
    let my_last = pr
        .comments
        .iter()
        .filter(|c| id.is_me(&c.author.login))
        .map(|c| c.created_at)
        .max();
    pr.comments
        .iter()
        .filter(|c| !id.is_me(&c.author.login) && !id.is_bot(&c.author) && my_last.is_none_or(|m| c.created_at > m))
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::{Author, ReviewState::*};
    use crate::domain::testkit::*;

    fn mark(login: &str, bot: bool, state: ReviewState, at: i64) -> ReviewMark {
        ReviewMark {
            author: if bot { Author::bot(login) } else { Author::user(login) },
            state,
            submitted_at: Some(t(at)),
        }
    }

    #[test]
    fn a_request_counts_as_reviewed_once_another_human_reviews() {
        assert!(reviewed_by_others(&[], &identity()).is_empty());
        let ignored = [
            mark("coderabbitai", true, Commented, 1),
            mark("me-home", false, ChangesRequested, 2),
            mark("carol", false, Dismissed, 3),
            mark("dave", false, Pending, 4),
        ];
        assert!(reviewed_by_others(&ignored, &identity()).is_empty());
        let reviews = [
            mark("raad", false, ChangesRequested, 1),
            mark("raad", false, Commented, 2),
            mark("alice", false, Commented, 3),
            mark("alice", false, Approved, 4),
            mark("bob", false, Commented, 5),
        ];
        let reasons = reviewed_by_others(&reviews, &identity());
        assert_eq!(
            reasons,
            vec![
                Reason::ChangesRequested {
                    by: vec!["raad".into()]
                },
                Reason::Approved {
                    by: vec!["alice".into()]
                },
                Reason::Reviewed { by: vec!["bob".into()] },
            ]
        );
        assert_eq!(describe(&reasons), "changes: raad · approved: alice · reviewed: bob");
    }

    #[test]
    fn clean_pr_has_no_reasons() {
        assert!(reasons(&my_pr("acme/api", 1), &identity()).is_empty());
    }

    #[test]
    fn unresolved_thread_from_someone_else_counts_until_i_have_the_last_word() {
        let mut pr = my_pr("acme/api", 1);
        pr.threads = vec![
            thread(false, "alice", "alice"),
            thread(false, "coderabbitai", "coderabbitai"),
            thread(false, "alice", "me-home"),
            thread(true, "bob", "bob"),
            thread(false, "me-work", "alice"),
        ];
        assert_eq!(reasons(&pr, &identity()), vec![Reason::Threads { count: 2 }]);
    }

    #[test]
    fn changes_requested_counts_only_on_current_head() {
        let mut pr = my_pr("acme/api", 1);
        pr.reviews = vec![
            review("r1", Author::user("alice"), ChangesRequested, 1, "head"),
            review("r2", Author::user("bob"), ChangesRequested, 1, "old"),
        ];
        assert_eq!(
            reasons(&pr, &identity()),
            vec![Reason::ChangesRequested {
                by: vec!["alice".to_string()]
            }]
        );
    }

    #[test]
    fn later_approval_overrides_changes_requested_but_comment_does_not() {
        let mut pr = my_pr("acme/api", 1);
        pr.reviews = vec![
            review("r1", Author::user("alice"), ChangesRequested, 1, "head"),
            review("r2", Author::user("alice"), Commented, 2, "head"),
            review("r3", Author::user("bob"), ChangesRequested, 1, "head"),
            review("r4", Author::user("bob"), Approved, 3, "head"),
        ];
        assert_eq!(
            reasons(&pr, &identity()),
            vec![Reason::ChangesRequested {
                by: vec!["alice".to_string()]
            }]
        );
    }

    #[test]
    fn human_comment_is_unanswered_until_i_comment_after_it() {
        let mut pr = my_pr("acme/api", 1);
        pr.comments = vec![
            comment("c1", Author::user("alice"), 1),
            comment("c2", Author::user("me-work"), 2),
            comment("c3", Author::user("bob"), 3),
            comment("c4", Author::bot("vercel"), 4),
            comment("c5", Author::user("ci-robot"), 5),
        ];
        assert_eq!(reasons(&pr, &identity()), vec![Reason::Comments { count: 1 }]);
    }

    #[test]
    fn describe_joins_all_reasons() {
        let text = describe(&[
            Reason::Threads { count: 3 },
            Reason::ChangesRequested {
                by: vec!["alice".into(), "bob".into()],
            },
            Reason::Comments { count: 1 },
        ]);
        assert_eq!(text, "3 threads · changes: alice, bob · 1 comment");
    }
}
