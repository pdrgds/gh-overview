use std::collections::HashSet;

use super::model::Author;

#[derive(Debug, Clone, Default)]
pub struct Identity {
    me: HashSet<String>,
    extra_bots: HashSet<String>,
}

impl Identity {
    pub fn new(
        me: impl IntoIterator<Item = impl AsRef<str>>,
        extra_bots: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        Identity {
            me: me.into_iter().map(|s| s.as_ref().to_lowercase()).collect(),
            extra_bots: extra_bots.into_iter().map(|s| s.as_ref().to_lowercase()).collect(),
        }
    }

    pub fn is_me(&self, login: &str) -> bool {
        self.me.contains(&login.to_lowercase())
    }

    pub fn is_bot(&self, author: &Author) -> bool {
        let login = author.login.to_lowercase();
        author.is_bot_type || login.ends_with("[bot]") || self.extra_bots.contains(&login)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> Identity {
        Identity::new(["Me-Work", "me-home"], ["ci-robot"])
    }

    #[test]
    fn me_matches_any_configured_account_case_insensitively() {
        assert!(id().is_me("me-work"));
        assert!(id().is_me("ME-HOME"));
        assert!(!id().is_me("alice"));
    }

    #[test]
    fn bot_detection_uses_type_suffix_and_extra_list() {
        assert!(id().is_bot(&Author::bot("coderabbitai")));
        assert!(id().is_bot(&Author::user("dependabot[bot]")));
        assert!(id().is_bot(&Author::user("CI-Robot")));
        assert!(!id().is_bot(&Author::user("alice")));
    }
}
