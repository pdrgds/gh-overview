use std::process::Command;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Browser {
    pub app: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

#[cfg(target_os = "macos")]
pub fn command(browser: &Browser, url: &str) -> Command {
    let mut command = Command::new("open");
    if browser.args.is_empty() {
        command.args(["-a", &browser.app, url]);
    } else {
        command
            .args(["-na", &browser.app, "--args"])
            .args(&browser.args)
            .arg(url);
    }
    command
}

#[cfg(not(target_os = "macos"))]
pub fn command(browser: &Browser, url: &str) -> Command {
    let mut command = Command::new(&browser.app);
    command.args(&browser.args).arg(url);
    command
}

pub fn open_url(browser: Option<&Browser>, url: &str) -> Result<()> {
    let Some(browser) = browser else {
        open::that(url)?;
        return Ok(());
    };
    let status = command(browser, url).status()?;
    if !status.success() {
        bail!("opening {url} with {} failed ({status})", browser.app);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_app_opens_with_open_and_profile_args_force_a_new_launch() {
        let url = "https://github.com/acme/api/pull/1";
        let plain = Browser {
            app: "Firefox".into(),
            args: vec![],
        };
        assert_eq!(args(&command(&plain, url)), ["open", "-a", "Firefox", url]);
        let profile = Browser {
            app: "Google Chrome".into(),
            args: vec!["--profile-directory=Profile 1".into()],
        };
        assert_eq!(
            args(&command(&profile, url)),
            [
                "open",
                "-na",
                "Google Chrome",
                "--args",
                "--profile-directory=Profile 1",
                url
            ]
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_browser_runs_with_its_args_then_the_url() {
        let browser = Browser {
            app: "firefox".into(),
            args: vec!["-P".into(), "work".into()],
        };
        assert_eq!(
            args(&command(&browser, "https://x")),
            ["firefox", "-P", "work", "https://x"]
        );
    }
}
