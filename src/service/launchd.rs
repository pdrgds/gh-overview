use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::ServiceInstaller;

pub const LABEL: &str = "dev.pdrgds.gh-overview";
const BOOTSTRAP_ATTEMPTS: u32 = 10;
const BOOTSTRAP_RETRY_DELAY: Duration = Duration::from_millis(300);

pub struct Launchd {
    plist_path: PathBuf,
}

impl Launchd {
    pub fn for_current_user() -> Result<Self> {
        let home = std::env::var("HOME").context("HOME is not set")?;
        Ok(Launchd {
            plist_path: PathBuf::from(home)
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
        })
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn plist(exe: &Path, log: &Path, env: &[(String, String)]) -> String {
    let exe = xml_escape(&exe.display().to_string());
    let log = xml_escape(&log.display().to_string());
    let env: String = env
        .iter()
        .map(|(name, value)| {
            format!(
                "    <key>{}</key>\n    <string>{}</string>\n",
                xml_escape(name),
                xml_escape(value)
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>daemon</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>30</integer>
  <key>EnvironmentVariables</key>
  <dict>
{env}  </dict>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#
    )
}

fn gui_domain() -> Result<String> {
    let out = Command::new("id").arg("-u").output().context("running `id -u`")?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || uid.is_empty() {
        bail!("could not determine the current user id");
    }
    Ok(format!("gui/{uid}"))
}

pub fn retry<T>(
    attempts: u32,
    delay: Duration,
    mut attempt: impl FnMut() -> std::result::Result<T, String>,
) -> std::result::Result<T, String> {
    let mut last = String::new();
    for n in 0..attempts {
        if n > 0 {
            std::thread::sleep(delay);
        }
        match attempt() {
            Ok(value) => return Ok(value),
            Err(err) => last = err,
        }
    }
    Err(last)
}

fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    Command::new("launchctl")
        .args(args)
        .output()
        .context("running launchctl")
}

impl ServiceInstaller for Launchd {
    fn install(&self, exe: &Path, log: &Path, env: &[(String, String)]) -> Result<()> {
        let domain = gui_domain()?;
        if let Some(dir) = self.plist_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.plist_path, plist(exe, log, env))?;
        launchctl(&["bootout", &format!("{domain}/{LABEL}")])?;
        let plist_path = self.plist_path.display().to_string();
        retry(BOOTSTRAP_ATTEMPTS, BOOTSTRAP_RETRY_DELAY, || {
            let out = launchctl(&["bootstrap", &domain, &plist_path]).map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
            }
        })
        .map_err(|err| anyhow::anyhow!("launchctl bootstrap failed: {err}"))
    }

    fn uninstall(&self) -> Result<()> {
        let domain = gui_domain()?;
        launchctl(&["bootout", &format!("{domain}/{LABEL}")])?;
        if self.plist_path.exists() {
            std::fs::remove_file(&self.plist_path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_runs_daemon_with_path_and_log() {
        let text = plist(
            Path::new("/Users/me/.cargo/bin/ghov"),
            Path::new("/Users/me/.local/state/gh-overview/daemon.log"),
            &[
                ("PATH".to_string(), "/opt/homebrew/bin:/usr/bin:/bin".to_string()),
                ("XDG_DATA_HOME".to_string(), "/Users/me/data".to_string()),
            ],
        );
        assert!(text.contains("<string>/Users/me/.cargo/bin/ghov</string>\n    <string>daemon</string>"));
        assert!(text.contains("<key>PATH</key>\n    <string>/opt/homebrew/bin:/usr/bin:/bin</string>"));
        assert!(text.contains(
            "<key>StandardErrorPath</key>\n  <string>/Users/me/.local/state/gh-overview/daemon.log</string>"
        ));
        assert!(text.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(text.contains("<key>ThrottleInterval</key>\n  <integer>30</integer>"));
        assert!(text.contains("<key>XDG_DATA_HOME</key>\n    <string>/Users/me/data</string>\n  </dict>"));
    }

    #[test]
    fn plist_escapes_xml() {
        let text = plist(
            Path::new("/a&b/ghov"),
            Path::new("/l"),
            &[("PATH".to_string(), "/x<y".to_string())],
        );
        assert!(text.contains("/a&amp;b/ghov"));
        assert!(text.contains("/x&lt;y"));
    }

    #[test]
    fn retry_succeeds_once_an_attempt_works() {
        let mut calls = 0;
        let result = retry(5, Duration::ZERO, || {
            calls += 1;
            if calls < 3 {
                Err(format!("busy {calls}"))
            } else {
                Ok(calls)
            }
        });
        assert_eq!(result, Ok(3));
    }

    #[test]
    fn retry_gives_up_with_the_last_error() {
        let mut calls = 0;
        let result: std::result::Result<(), String> = retry(4, Duration::ZERO, || {
            calls += 1;
            Err(format!("Bootstrap failed: 5: Input/output error ({calls})"))
        });
        assert_eq!(result, Err("Bootstrap failed: 5: Input/output error (4)".to_string()));
        assert_eq!(calls, 4);
    }
}
