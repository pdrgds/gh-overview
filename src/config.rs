use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::browser::Browser;
use crate::domain::actors::Identity;
use crate::domain::snooze::SnoozeChoice;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub login: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<Browser>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(with = "humantime_serde", default = "default_poll")]
    pub poll_interval: Duration,
    #[serde(with = "humantime_serde", default = "default_renotify")]
    pub renotify_interval: Duration,
    #[serde(with = "humantime_serde", default = "default_remind_after_open")]
    pub remind_after_open: Duration,
    #[serde(default = "default_tomorrow_hour")]
    pub tomorrow_hour: u32,
    #[serde(default = "default_snooze")]
    pub snooze_choices: Vec<String>,
    #[serde(default)]
    pub extra_bots: Vec<String>,
    #[serde(default)]
    pub notify_team_requests: bool,
    pub accounts: Vec<Account>,
}

fn default_poll() -> Duration {
    Duration::from_secs(60)
}

fn default_renotify() -> Duration {
    Duration::from_secs(300)
}

fn default_remind_after_open() -> Duration {
    Duration::from_secs(30 * 60)
}

fn default_tomorrow_hour() -> u32 {
    9
}

fn default_snooze() -> Vec<String> {
    vec!["15m".into(), "1h".into(), "tomorrow".into()]
}

impl Config {
    pub fn with_accounts(logins: Vec<String>) -> Self {
        Config {
            poll_interval: default_poll(),
            renotify_interval: default_renotify(),
            remind_after_open: default_remind_after_open(),
            tomorrow_hour: default_tomorrow_hour(),
            snooze_choices: default_snooze(),
            extra_bots: vec![],
            notify_team_requests: false,
            accounts: logins
                .into_iter()
                .map(|login| Account {
                    label: login.clone(),
                    login,
                    browser: None,
                })
                .collect(),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("config always serializes")
    }

    fn validate(&self) -> Result<()> {
        if self.accounts.is_empty() {
            bail!("config needs at least one [[accounts]] entry");
        }
        if self.poll_interval < Duration::from_secs(10) {
            bail!("poll_interval must be at least 10s");
        }
        if self.renotify_interval < Duration::from_secs(60) {
            bail!("renotify_interval must be at least 1m");
        }
        if self.remind_after_open < Duration::from_secs(60) {
            bail!("remind_after_open must be at least 1m");
        }
        if self.tomorrow_hour > 23 {
            bail!("tomorrow_hour must be 0..=23, got {}", self.tomorrow_hour);
        }
        if self.snooze_choices.is_empty() || self.snooze_choices.len() > 9 {
            bail!("snooze_choices needs 1 to 9 entries");
        }
        let mut labels = Vec::new();
        for raw in &self.snooze_choices {
            let label = SnoozeChoice::parse(raw)
                .map_err(anyhow::Error::msg)?
                .label(self.tomorrow_hour);
            if labels.contains(&label) {
                bail!("snooze_choices lists {raw:?} twice");
            }
            labels.push(label);
        }
        Ok(())
    }

    pub fn identity(&self) -> Identity {
        Identity::new(self.accounts.iter().map(|a| &a.login), &self.extra_bots)
    }

    pub fn snooze(&self) -> Vec<SnoozeChoice> {
        self.snooze_choices
            .iter()
            .map(|raw| SnoozeChoice::parse(raw).expect("validated on load"))
            .collect()
    }

    fn account(&self, login: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.login.eq_ignore_ascii_case(login))
    }

    pub fn label_for<'a>(&'a self, login: &'a str) -> &'a str {
        self.account(login).map_or(login, |a| a.label.as_str())
    }

    pub fn browser_for(&self, login: &str) -> Option<&Browser> {
        self.account(login).and_then(|a| a.browser.as_ref())
    }

    pub fn poll(&self) -> chrono::Duration {
        chrono::Duration::from_std(self.poll_interval).expect("poll interval fits")
    }

    pub fn renotify(&self) -> chrono::Duration {
        chrono::Duration::from_std(self.renotify_interval).expect("renotify interval fits")
    }

    pub fn remind_after_open(&self) -> chrono::Duration {
        chrono::Duration::from_std(self.remind_after_open).expect("reminder interval fits")
    }
}

pub fn load_or_create(path: &Path, discover: impl FnOnce() -> Result<Vec<String>>) -> Result<Config> {
    if path.exists() {
        let text = std::fs::read_to_string(path)?;
        return Config::parse(&text).with_context(|| format!("invalid config {}", path.display()));
    }
    let logins = discover()?;
    if logins.is_empty() {
        bail!("no GitHub accounts found in `gh auth status`; run `gh auth login` first");
    }
    let config = Config::with_accounts(logins);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, config.to_toml())?;
    Ok(config)
}

pub fn discover_gh_accounts() -> Result<Vec<String>> {
    let out = Command::new("gh")
        .args(["auth", "status", "--json", "hosts"])
        .output()
        .context("running `gh auth status`")?;
    parse_gh_auth_status(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_gh_auth_status(json: &str) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct Status {
        hosts: HashMap<String, Vec<HostAccount>>,
    }
    #[derive(Deserialize)]
    struct HostAccount {
        login: String,
        state: String,
    }
    let status: Status = serde_json::from_str(json).context("parsing `gh auth status --json hosts`")?;
    Ok(status
        .hosts
        .get("github.com")
        .map(|accounts| {
            accounts
                .iter()
                .filter(|a| a.state == "success")
                .map(|a| a.login.clone())
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
poll_interval = "90s"
renotify_interval = "10m"
remind_after_open = "45m"
tomorrow_hour = 8
snooze_choices = ["30m", "tomorrow"]
extra_bots = ["ci-robot"]

[[accounts]]
login = "octocat-work"
label = "work"
browser = { app = "Google Chrome", args = ["--profile-directory=Profile 1"] }

[[accounts]]
login = "octocat"
label = "personal"
"#;

    #[test]
    fn parses_full_config() {
        let c = Config::parse(FULL).unwrap();
        assert_eq!(c.poll_interval, Duration::from_secs(90));
        assert_eq!(c.renotify_interval, Duration::from_secs(600));
        assert_eq!(c.remind_after_open, Duration::from_secs(45 * 60));
        assert_eq!(c.tomorrow_hour, 8);
        assert_eq!(c.snooze().len(), 2);
        assert_eq!(c.label_for("OCTOCAT"), "personal");
        assert_eq!(c.label_for("stranger"), "stranger");
        assert_eq!(
            c.browser_for("OCTOCAT-WORK"),
            Some(&Browser {
                app: "Google Chrome".into(),
                args: vec!["--profile-directory=Profile 1".into()],
            })
        );
        assert_eq!(c.browser_for("octocat"), None);
        assert_eq!(Config::parse(&c.to_toml()).unwrap(), c);
        assert!(c.identity().is_me("octocat-work"));
    }

    #[test]
    fn defaults_apply_when_only_accounts_given() {
        let c = Config::parse("[[accounts]]\nlogin = \"a\"\nlabel = \"a\"\n").unwrap();
        assert_eq!(c.poll_interval, Duration::from_secs(60));
        assert_eq!(c.renotify_interval, Duration::from_secs(300));
        assert_eq!(c.remind_after_open, Duration::from_secs(30 * 60));
        assert_eq!(c.snooze_choices, vec!["15m", "1h", "tomorrow"]);
    }

    #[test]
    fn rejects_invalid_config() {
        assert!(Config::parse("accounts = []").is_err());
        assert!(Config::parse("renotify_interval = \"0s\"\n[[accounts]]\nlogin = \"a\"\nlabel = \"a\"\n").is_err());
        assert!(Config::parse("remind_after_open = \"10s\"\n[[accounts]]\nlogin = \"a\"\nlabel = \"a\"\n").is_err());
        assert!(Config::parse("poll_interval = \"1s\"\n[[accounts]]\nlogin = \"a\"\nlabel = \"a\"\n").is_err());
        assert!(Config::parse("tomorrow_hour = 24\n[[accounts]]\nlogin = \"a\"\nlabel = \"a\"\n").is_err());
        assert!(Config::parse("snooze_choices = [\"soon\"]\n[[accounts]]\nlogin = \"a\"\nlabel = \"a\"\n").is_err());
        for dupes in [r#"["1h", "1h"]"#, r#"["1h", " 1h"]"#, r#"["tomorrow", "Tomorrow"]"#] {
            let toml = format!("snooze_choices = {dupes}\n[[accounts]]\nlogin = \"a\"\nlabel = \"a\"\n");
            assert!(Config::parse(&toml).is_err(), "{dupes} should be rejected");
        }
    }

    #[test]
    fn generated_config_round_trips() {
        let c = Config::with_accounts(vec!["octocat-work".into(), "octocat".into()]);
        assert_eq!(Config::parse(&c.to_toml()).unwrap(), c);
    }

    #[test]
    fn load_or_create_writes_discovered_accounts_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let c = load_or_create(&path, || Ok(vec!["octocat".into()])).unwrap();
        assert_eq!(c.accounts[0].login, "octocat");
        let again = load_or_create(&path, || panic!("must not rediscover")).unwrap();
        assert_eq!(again, c);
    }

    #[test]
    fn parses_gh_auth_status_successful_github_accounts() {
        let json = r#"{"hosts":{"github.com":[
            {"state":"success","active":true,"host":"github.com","login":"octocat-work"},
            {"state":"error","active":false,"host":"github.com","login":"broken"},
            {"state":"success","active":false,"host":"github.com","login":"octocat"}]}}"#;
        assert_eq!(parse_gh_auth_status(json).unwrap(), vec!["octocat-work", "octocat"]);
    }
}
