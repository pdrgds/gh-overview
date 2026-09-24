use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::json;

use super::parse::parse_response;
use super::token::TokenProvider;
use super::{FetchError, MINE_SEARCH, QUERY, REVIEW_SEARCH};
use crate::domain::model::AccountSnapshot;

pub trait GithubSource {
    fn fetch(&self, login: &str) -> Result<AccountSnapshot, FetchError>;
}

pub struct GithubClient<T: TokenProvider> {
    http: reqwest::blocking::Client,
    tokens: T,
    cache: Mutex<HashMap<String, String>>,
}

impl<T: TokenProvider> GithubClient<T> {
    pub fn new(tokens: T) -> anyhow::Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("gh-overview/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(GithubClient {
            http,
            tokens,
            cache: Mutex::new(HashMap::new()),
        })
    }

    fn token(&self, login: &str, refresh: bool) -> Result<String, FetchError> {
        if !refresh && let Some(token) = self.cache.lock().expect("token cache poisoned").get(login).cloned() {
            return Ok(token);
        }
        let token = self.tokens.token(login)?;
        self.cache
            .lock()
            .expect("token cache poisoned")
            .insert(login.to_string(), token.clone());
        Ok(token)
    }

    fn post(&self, token: &str) -> Result<(u16, String), FetchError> {
        let body = json!({
            "query": QUERY,
            "variables": { "mine": MINE_SEARCH, "review": REVIEW_SEARCH },
        });
        let response = self
            .http
            .post("https://api.github.com/graphql")
            .bearer_auth(token)
            .json(&body)
            .send()
            .map_err(|e| FetchError::Http(e.to_string()))?;
        let status = response.status().as_u16();
        let text = response.text().map_err(|e| FetchError::Http(e.to_string()))?;
        Ok((status, text))
    }
}

impl<T: TokenProvider> GithubSource for GithubClient<T> {
    fn fetch(&self, login: &str) -> Result<AccountSnapshot, FetchError> {
        let (mut status, mut body) = self.post(&self.token(login, false)?)?;
        if status == 401 {
            (status, body) = self.post(&self.token(login, true)?)?;
        }
        match status {
            200 => parse_response(login, &body),
            401 => Err(FetchError::Unauthorized),
            other => Err(FetchError::Http(format!(
                "status {other}: {}",
                body.chars().take(200).collect::<String>()
            ))),
        }
    }
}
