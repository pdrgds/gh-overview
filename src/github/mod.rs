pub mod client;
pub mod parse;
pub mod token;

pub const QUERY: &str = include_str!("query.graphql");
pub const MINE_SEARCH: &str = "is:pr is:open archived:false author:@me sort:updated-desc";
pub const REVIEW_SEARCH: &str = "is:pr is:open archived:false review-requested:@me sort:updated-desc";

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("gh auth token failed: {0}")]
    Token(String),
    #[error("unauthorized (401)")]
    Unauthorized,
    #[error("http error: {0}")]
    Http(String),
    #[error("graphql error: {0}")]
    Graphql(String),
    #[error("unexpected response: {0}")]
    Parse(String),
}
