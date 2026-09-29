use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::FetchError;
use crate::domain::model::{
    AccountSnapshot, Author, Comment, MyPr, PrBase, RequestEvent, Review, ReviewRequest, ReviewState, Thread,
};

#[derive(Deserialize)]
struct Envelope {
    data: Option<Data>,
    #[serde(default)]
    errors: Vec<GqlError>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    rate_limit: Option<RateLimit>,
    mine: Search<RawMyPr>,
    review: Search<RawReviewPr>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimit {
    remaining: u32,
    reset_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Search<T> {
    issue_count: u32,
    nodes: Vec<Option<T>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Conn<T> {
    #[serde(default)]
    page_info: PageInfo,
    nodes: Vec<Option<T>>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    #[serde(default)]
    has_previous_page: bool,
}

#[derive(Deserialize)]
struct RawActor {
    #[serde(rename = "__typename", default)]
    typename: Option<String>,
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBase {
    number: u64,
    title: String,
    url: String,
    is_draft: bool,
    created_at: DateTime<Utc>,
    repository: RawRepo,
    author: Option<RawActor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRepo {
    name_with_owner: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMyPr {
    #[serde(flatten)]
    base: RawBase,
    head_ref_oid: String,
    reviews: Conn<RawReview>,
    review_threads: Conn<RawThread>,
    comments: Conn<RawComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReview {
    id: String,
    state: ReviewState,
    submitted_at: Option<DateTime<Utc>>,
    author: Option<RawActor>,
    commit: Option<RawCommit>,
    comments: Count,
}

#[derive(Deserialize)]
struct RawCommit {
    oid: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Count {
    total_count: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThread {
    is_resolved: bool,
    first_comment: Conn<RawCommentAuthor>,
    last_comment: Conn<RawCommentAuthor>,
}

#[derive(Deserialize)]
struct RawCommentAuthor {
    author: Option<RawActor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawComment {
    id: String,
    created_at: DateTime<Utc>,
    author: Option<RawActor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReviewPr {
    #[serde(flatten)]
    base: RawBase,
    review_requests: Conn<RawRequest>,
    #[serde(default)]
    timeline_items: Option<Conn<RawRequestEvent>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRequestEvent {
    id: Option<String>,
    actor: Option<RawActor>,
    requested_reviewer: Option<RawReviewer>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRequest {
    requested_reviewer: Option<RawReviewer>,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum RawReviewer {
    User {
        login: String,
    },
    Team {
        slug: String,
    },
    #[serde(other)]
    Other,
}

const GHOST: &str = "ghost";

fn author(actor: Option<RawActor>) -> Author {
    match actor {
        Some(a) => Author {
            is_bot_type: a.typename.as_deref() == Some("Bot"),
            login: a.login,
        },
        None => Author::user(GHOST),
    }
}

fn base(raw: RawBase) -> PrBase {
    PrBase {
        repo: raw.repository.name_with_owner,
        number: raw.number,
        title: raw.title,
        url: raw.url,
        author_is_bot: raw
            .author
            .as_ref()
            .is_some_and(|a| a.typename.as_deref() == Some("Bot")),
        author: raw.author.map_or_else(|| GHOST.to_string(), |a| a.login),
        is_draft: raw.is_draft,
        created_at: raw.created_at,
    }
}

fn first_login(conn: Conn<RawCommentAuthor>) -> Option<String> {
    conn.nodes.into_iter().flatten().next().map(|c| author(c.author).login)
}

fn my_pr(raw: RawMyPr, truncated: &mut Vec<String>) -> MyPr {
    let base = base(raw.base);
    let key = base.key();
    if raw.reviews.page_info.has_previous_page {
        truncated.push(format!("{key}: more than 50 reviews, older ones ignored"));
    }
    if raw.review_threads.page_info.has_previous_page {
        truncated.push(format!("{key}: more than 40 review threads, older ones ignored"));
    }
    if raw.comments.page_info.has_previous_page {
        truncated.push(format!("{key}: more than 50 comments, older ones ignored"));
    }
    MyPr {
        head_oid: raw.head_ref_oid,
        reviews: raw
            .reviews
            .nodes
            .into_iter()
            .flatten()
            .map(|r| Review {
                id: r.id,
                author: author(r.author),
                state: r.state,
                submitted_at: r.submitted_at,
                commit_oid: r.commit.map(|c| c.oid),
                comment_count: r.comments.total_count,
            })
            .collect(),
        threads: raw
            .review_threads
            .nodes
            .into_iter()
            .flatten()
            .map(|t| Thread {
                is_resolved: t.is_resolved,
                first_author: first_login(t.first_comment),
                last_author: first_login(t.last_comment),
            })
            .collect(),
        comments: raw
            .comments
            .nodes
            .into_iter()
            .flatten()
            .map(|c| Comment {
                id: c.id,
                author: author(c.author),
                created_at: c.created_at,
            })
            .collect(),
        base,
    }
}

fn review_request(raw: RawReviewPr, login: &str) -> ReviewRequest {
    let mut direct = false;
    let mut team = None;
    for request in raw.review_requests.nodes.into_iter().flatten() {
        match request.requested_reviewer {
            Some(RawReviewer::User { login: l }) if l.eq_ignore_ascii_case(login) => direct = true,
            Some(RawReviewer::Team { slug }) if team.is_none() => team = Some(slug),
            _ => {}
        }
    }
    let events = raw.timeline_items.map(|c| c.nodes).unwrap_or_default();
    let event = events.into_iter().flatten().rev().find_map(|e| {
        let asked_me = match (&e.requested_reviewer, &team) {
            (Some(RawReviewer::User { login: l }), _) => direct && l.eq_ignore_ascii_case(login),
            (Some(RawReviewer::Team { slug }), Some(wanted)) => !direct && slug == wanted,
            _ => false,
        };
        if !asked_me {
            return None;
        }
        Some(RequestEvent {
            id: e.id?,
            actor: author(e.actor).login,
        })
    });
    ReviewRequest {
        base: base(raw.base),
        direct,
        team,
        event,
    }
}

pub fn parse_response(login: &str, body: &str) -> Result<AccountSnapshot, FetchError> {
    let envelope: Envelope = serde_json::from_str(body).map_err(|e| FetchError::Parse(e.to_string()))?;
    let errors: Vec<String> = envelope.errors.into_iter().map(|e| e.message).collect();
    let Some(data) = envelope.data else {
        return Err(FetchError::Graphql(errors.join("; ")));
    };
    let mut truncated = Vec::new();
    let withheld_mine = data.mine.nodes.iter().any(Option::is_none);
    let withheld_review = data.review.nodes.iter().any(Option::is_none);
    let mine_nodes = data.mine.nodes.len() as u32;
    if data.mine.issue_count > mine_nodes {
        truncated.push(format!(
            "{} open PRs authored, only {mine_nodes} fetched",
            data.mine.issue_count
        ));
    }
    let review_nodes = data.review.nodes.len() as u32;
    if data.review.issue_count > review_nodes {
        truncated.push(format!(
            "{} review requests, only {review_nodes} fetched",
            data.review.issue_count
        ));
    }
    let mine = data
        .mine
        .nodes
        .into_iter()
        .flatten()
        .map(|pr| my_pr(pr, &mut truncated))
        .collect();
    let to_review = data
        .review
        .nodes
        .into_iter()
        .flatten()
        .map(|pr| review_request(pr, login))
        .collect();
    Ok(AccountSnapshot {
        login: login.to_string(),
        mine,
        to_review,
        rate_remaining: data.rate_limit.as_ref().map(|r| r.remaining),
        rate_reset_at: data.rate_limit.map(|r| r.reset_at),
        errors,
        truncated,
        withheld_mine,
        withheld_review,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/overview.json");

    #[test]
    fn parses_my_prs() {
        let snap = parse_response("me-work", FIXTURE).unwrap();
        assert_eq!(snap.rate_remaining, Some(4609));
        assert_eq!(snap.mine.len(), 2);
        let pr = &snap.mine[0];
        assert_eq!(pr.base.key(), "acme/api#412");
        assert_eq!(pr.head_oid, "abc123");
        assert_eq!(pr.reviews[0].author, Author::bot("coderabbitai"));
        assert_eq!(pr.reviews[0].comment_count, 3);
        assert_eq!(pr.reviews[1].state, ReviewState::ChangesRequested);
        assert_eq!(pr.reviews[1].commit_oid.as_deref(), Some("abc123"));
        assert_eq!(
            pr.threads[1],
            Thread {
                is_resolved: true,
                first_author: Some("alice".into()),
                last_author: Some("me-work".into())
            }
        );
        assert_eq!(pr.comments[1].author, Author::user("ghost"));
        assert!(snap.mine[1].base.is_draft);
    }

    #[test]
    fn flags_truncated_connections() {
        let snap = parse_response("me-work", FIXTURE).unwrap();
        assert_eq!(
            snap.truncated,
            vec![
                "acme/web#9: more than 50 reviews, older ones ignored",
                "acme/web#9: more than 40 review threads, older ones ignored",
            ]
        );
    }

    #[test]
    fn classifies_review_requests_as_direct_or_team() {
        let snap = parse_response("me-work", FIXTURE).unwrap();
        let team = &snap.to_review[0];
        assert_eq!((team.direct, team.team.as_deref()), (false, Some("platform")));
        assert!(team.base.author_is_bot);
        assert_eq!(
            team.event,
            Some(RequestEvent {
                id: "RRE_old".into(),
                actor: "carol".into()
            })
        );
        let direct = &snap.to_review[1];
        assert_eq!((direct.direct, direct.team.as_deref()), (true, None));
        assert_eq!(direct.base.author, "dave");
        assert!(!direct.base.author_is_bot);
        assert_eq!(
            direct.event,
            Some(RequestEvent {
                id: "RRE_again".into(),
                actor: "erin".into()
            })
        );
    }

    #[test]
    fn flags_review_requests_beyond_the_search_limit() {
        let body = FIXTURE.replacen(
            "\"issueCount\": 2,\n      \"nodes\": [\n        {\n          \"number\": 77",
            "\"issueCount\": 45,\n      \"nodes\": [\n        {\n          \"number\": 77",
            1,
        );
        let snap = parse_response("me-work", &body).unwrap();
        assert!(
            snap.truncated
                .contains(&"45 review requests, only 2 fetched".to_string())
        );
        assert!(!snap.withheld_review);
        assert!(!snap.withheld_mine);
    }

    #[test]
    fn partial_errors_are_kept_with_data() {
        let body = FIXTURE.replacen(
            "{\n  \"data\"",
            "{\n  \"errors\": [{\"message\": \"SSO required for org x\"}],\n  \"data\"",
            1,
        );
        let snap = parse_response("me-work", &body).unwrap();
        assert_eq!(snap.errors, vec!["SSO required for org x"]);
    }

    #[test]
    fn errors_without_data_fail() {
        let err = parse_response("me-work", r#"{"data":null,"errors":[{"message":"Bad credentials"}]}"#).unwrap_err();
        assert!(matches!(err, FetchError::Graphql(m) if m == "Bad credentials"));
    }

    #[test]
    fn flags_searches_with_withheld_results() {
        let body = FIXTURE.replacen(
            "\"nodes\": [\n        {\n          \"number\": 412",
            "\"nodes\": [\n        null,\n        {\n          \"number\": 412",
            1,
        );
        let snap = parse_response("me-work", &body).unwrap();
        assert_eq!((snap.withheld_mine, snap.withheld_review), (true, false));
        assert_eq!(snap.mine.len(), 2);
        let clean = parse_response("me-work", FIXTURE).unwrap();
        assert_eq!((clean.withheld_mine, clean.withheld_review), (false, false));
    }

    #[test]
    fn unknown_review_states_do_not_break_parsing() {
        let body = FIXTURE.replacen("\"state\": \"CHANGES_REQUESTED\"", "\"state\": \"SOMETHING_NEW\"", 1);
        let snap = parse_response("me-work", &body).unwrap();
        assert_eq!(snap.mine[0].reviews[1].state, ReviewState::Unknown);
    }
}
