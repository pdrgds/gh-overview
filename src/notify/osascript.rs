use std::process::{Command, Stdio};

use super::{Notification, Notifier};

pub struct OsascriptNotifier {
    reason: Option<String>,
}

impl OsascriptNotifier {
    pub fn fallback() -> Self {
        OsascriptNotifier { reason: None }
    }

    pub fn because(reason: impl Into<String>) -> Self {
        OsascriptNotifier {
            reason: Some(reason.into()),
        }
    }
}

const SCRIPT: [&str; 6] = [
    "-e",
    "on run argv",
    "-e",
    "display notification (item 1 of argv) with title (item 2 of argv) subtitle (item 3 of argv)",
    "-e",
    "end run",
];

impl Notifier for OsascriptNotifier {
    fn show(&mut self, n: &Notification) -> anyhow::Result<()> {
        Command::new("osascript")
            .args(SCRIPT)
            .args([&n.message, &n.title, &n.subtitle])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        Ok(())
    }

    fn remove(&mut self, _pr_key: &str) -> anyhow::Result<()> {
        Ok(())
    }

    fn degraded(&self) -> Option<String> {
        self.reason.as_ref().map(|reason| {
            format!("{reason}; notifications fall back to osascript and may not appear (snooze only via TUI)")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_top_level_fallback_reports_why() {
        assert_eq!(OsascriptNotifier::fallback().degraded(), None);
        let degraded = OsascriptNotifier::because("the notifier app is not installed")
            .degraded()
            .unwrap();
        assert!(degraded.starts_with("the notifier app is not installed; "));
        assert!(degraded.contains("snooze only via TUI"));
    }
}
