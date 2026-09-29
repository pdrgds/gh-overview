use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::actors::Identity;
use super::model::{MyPr, ReviewState};
use super::reasons::plural;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Review,
    Comment,
    ReviewRequest,
    TeamReviewRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityItem {
    pub id: String,
    pub kind: ActivityKind,
    pub actor: String,
    pub review_state: Option<ReviewState>,
    pub comment_count: u32,
    pub at: DateTime<Utc>,
}

pub fn activity_of(pr: &MyPr, id: &Identity) -> Vec<ActivityItem> {
    let reviews = pr
        .reviews
        .iter()
        .filter(|r| !id.is_me(&r.author.login) && r.state != ReviewState::Pending)
        .filter(|r| !(id.is_bot(&r.author) && r.state == ReviewState::Commented && r.comment_count == 0))
        .filter_map(|r| {
            r.submitted_at.map(|at| ActivityItem {
                id: r.id.clone(),
                kind: ActivityKind::Review,
                actor: r.author.login.clone(),
                review_state: Some(r.state),
                comment_count: r.comment_count,
                at,
            })
        });
    let comments = pr
        .comments
        .iter()
        .filter(|c| !id.is_me(&c.author.login) && !id.is_bot(&c.author))
        .map(|c| ActivityItem {
            id: c.id.clone(),
            kind: ActivityKind::Comment,
            actor: c.author.login.clone(),
            review_state: None,
            comment_count: 1,
            at: c.created_at,
        });
    let mut items: Vec<ActivityItem> = reviews.chain(comments).collect();
    items.sort_by_key(|a| a.at);
    items
}

pub fn summarize(items: &[ActivityItem]) -> String {
    let mut actors: Vec<&str> = Vec::new();
    for item in items {
        if !actors.contains(&item.actor.as_str()) {
            actors.push(&item.actor);
        }
    }
    actors
        .iter()
        .map(|actor| {
            let theirs: Vec<&ActivityItem> = items.iter().filter(|i| i.actor == *actor).collect();
            if theirs.iter().any(|i| i.kind == ActivityKind::ReviewRequest) {
                return format!("{actor} requested your review");
            }
            if theirs.iter().any(|i| i.kind == ActivityKind::TeamReviewRequest) {
                return format!("{actor} requested a review from your team");
            }
            let latest_decision = theirs.iter().rev().find_map(|i| match i.review_state {
                Some(state @ (ReviewState::ChangesRequested | ReviewState::Approved)) => Some(state),
                _ => None,
            });
            match latest_decision {
                Some(ReviewState::ChangesRequested) => format!("{actor} requested changes"),
                Some(_) => format!("{actor} approved"),
                None => {
                    let n: u32 = theirs.iter().map(|i| i.comment_count).sum();
                    if n == 0 {
                        format!("{actor} reviewed")
                    } else {
                        format!("{actor}: {}", plural(n, "comment"))
                    }
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::{Author, ReviewState::*};
    use crate::domain::testkit::*;

    #[test]
    fn review_requests_are_summarized_by_who_asked() {
        let request = |id: &str, kind| ActivityItem {
            id: id.to_string(),
            kind,
            actor: "dave".into(),
            review_state: None,
            comment_count: 0,
            at: t(0),
        };
        assert_eq!(
            summarize(&[request("rr:1", ActivityKind::ReviewRequest)]),
            "dave requested your review"
        );
        assert_eq!(
            summarize(&[request("rr:2", ActivityKind::TeamReviewRequest)]),
            "dave requested a review from your team"
        );
    }

    #[test]
    fn activity_includes_bot_reviews_but_only_human_comments_and_never_mine() {
        let mut pr = my_pr("acme/api", 1);
        let mut rabbit = review("r1", Author::bot("coderabbitai"), Commented, 2, "head");
        rabbit.comment_count = 2;
        pr.reviews = vec![
            rabbit,
            review("r2", Author::user("me-home"), Commented, 3, "head"),
            review("r3", Author::user("alice"), Pending, 4, "head"),
            review("r4", Author::bot("coderabbitai"), Commented, 7, "head"),
        ];
        pr.comments = vec![
            comment("c1", Author::user("bob"), 1),
            comment("c2", Author::bot("vercel"), 5),
            comment("c3", Author::user("me-work"), 6),
        ];
        let ids: Vec<String> = activity_of(&pr, &identity()).into_iter().map(|i| i.id).collect();
        assert_eq!(ids, vec!["c1", "r1"]);
    }

    #[test]
    fn summary_uses_each_actors_latest_decision() {
        let mut pr = my_pr("acme/api", 1);
        pr.reviews = vec![
            review("r1", Author::user("alice"), ChangesRequested, 1, "old"),
            review("r2", Author::user("alice"), Approved, 5, "head"),
            review("r3", Author::user("bob"), Approved, 2, "old"),
            review("r4", Author::user("bob"), ChangesRequested, 6, "head"),
        ];
        let text = summarize(&activity_of(&pr, &identity()));
        assert_eq!(text, "alice approved · bob requested changes");
    }

    #[test]
    fn summary_groups_by_actor_in_first_appearance_order() {
        let mut pr = my_pr("acme/api", 1);
        let mut rabbit = review("r1", Author::bot("coderabbitai"), Commented, 1, "head");
        rabbit.comment_count = 3;
        pr.reviews = vec![
            rabbit,
            review("r2", Author::user("alice"), Commented, 2, "head"),
            review("r3", Author::user("alice"), ChangesRequested, 3, "head"),
            review("r4", Author::user("bob"), Approved, 4, "head"),
        ];
        pr.comments = vec![comment("c1", Author::user("carol"), 5)];
        let text = summarize(&activity_of(&pr, &identity()));
        assert_eq!(
            text,
            "coderabbitai: 3 comments · alice requested changes · bob approved · carol: 1 comment"
        );
    }

    #[test]
    fn empty_bot_reviews_are_ignored_but_empty_human_reviews_read_as_reviewed() {
        let mut pr = my_pr("acme/api", 1);
        pr.reviews = vec![
            review("r1", Author::bot("coderabbitai"), Commented, 1, "head"),
            review("r2", Author::bot("coderabbitai"), Approved, 2, "head"),
            review("r3", Author::user("dave"), Commented, 3, "head"),
        ];
        assert_eq!(
            summarize(&activity_of(&pr, &identity())),
            "coderabbitai approved · dave reviewed"
        );
    }
}
