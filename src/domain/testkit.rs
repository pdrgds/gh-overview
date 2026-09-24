use chrono::{DateTime, Duration, TimeZone, Utc};

use super::actors::Identity;
use super::model::{Author, Comment, MyPr, PrBase, Review, ReviewState, Thread};

pub fn t(minutes: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 21, 10, 0, 0).unwrap() + Duration::minutes(minutes)
}

pub fn identity() -> Identity {
    Identity::new(["me-work", "me-home"], ["ci-robot"])
}

pub fn base(repo: &str, number: u64) -> PrBase {
    PrBase {
        repo: repo.to_string(),
        number,
        title: format!("PR {number}"),
        url: format!("https://github.com/{repo}/pull/{number}"),
        author: "me-work".to_string(),
        is_draft: false,
        created_at: t(-600),
    }
}

pub fn my_pr(repo: &str, number: u64) -> MyPr {
    MyPr {
        base: base(repo, number),
        head_oid: "head".to_string(),
        reviews: vec![],
        threads: vec![],
        comments: vec![],
    }
}

pub fn review(id: &str, author: Author, state: ReviewState, at: i64, oid: &str) -> Review {
    Review {
        id: id.to_string(),
        author,
        state,
        submitted_at: Some(t(at)),
        commit_oid: Some(oid.to_string()),
        comment_count: 0,
    }
}

pub fn thread(resolved: bool, first: &str, last: &str) -> Thread {
    Thread {
        is_resolved: resolved,
        first_author: Some(first.to_string()),
        last_author: Some(last.to_string()),
    }
}

pub fn comment(id: &str, author: Author, at: i64) -> Comment {
    Comment {
        id: id.to_string(),
        author,
        created_at: t(at),
    }
}
