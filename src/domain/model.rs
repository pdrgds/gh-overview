use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub fn pr_key(repo: &str, number: u64) -> String {
    format!("{repo}#{number}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub login: String,
    pub is_bot_type: bool,
}

impl Author {
    pub fn user(login: &str) -> Self {
        Author {
            login: login.to_string(),
            is_bot_type: false,
        }
    }

    pub fn bot(login: &str) -> Self {
        Author {
            login: login.to_string(),
            is_bot_type: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub id: String,
    pub author: Author,
    pub state: ReviewState,
    pub submitted_at: Option<DateTime<Utc>>,
    pub commit_oid: Option<String>,
    pub comment_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub is_resolved: bool,
    pub first_author: Option<String>,
    pub last_author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub id: String,
    pub author: Author,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrBase {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub author_is_bot: bool,
    pub is_draft: bool,
    pub created_at: DateTime<Utc>,
}

impl PrBase {
    pub fn key(&self) -> String {
        pr_key(&self.repo, self.number)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MyPr {
    pub base: PrBase,
    pub head_oid: String,
    pub reviews: Vec<Review>,
    pub threads: Vec<Thread>,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRequest {
    pub base: PrBase,
    pub direct: bool,
    pub team: Option<String>,
    pub event: Option<RequestEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestEvent {
    pub id: String,
    pub actor: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountSnapshot {
    pub login: String,
    pub mine: Vec<MyPr>,
    pub to_review: Vec<ReviewRequest>,
    pub rate_remaining: Option<u32>,
    pub rate_reset_at: Option<DateTime<Utc>>,
    pub errors: Vec<String>,
    pub truncated: Vec<String>,
    pub withheld_mine: bool,
    pub withheld_review: bool,
}
