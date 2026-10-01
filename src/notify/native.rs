use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::{Delivered, MUTE_TITLE, Notification, Notifier, Response};

const DENIED: &str =
    "notifications are off for gh-overview; allow them in System Settings → Notifications (snooze only via TUI)";
const PENDING: &str = "gh-overview is waiting for notification permission; choose Options → Allow on its prompt or allow it in System Settings → Notifications";
const STOPPED: &str =
    "the gh-overview notifier app is not running and is retried; see the daemon log (osascript fallback meanwhile)";
const RESPAWN_GAP: Duration = Duration::from_secs(30);
const STABLE_RUN: Duration = Duration::from_secs(300);

#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request<'a> {
    Show {
        key: &'a str,
        generation: u64,
        title: &'a str,
        subtitle: &'a str,
        message: &'a str,
        snoozable: bool,
    },
    Remove {
        key: &'a str,
    },
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authorization {
    #[serde(skip_deserializing)]
    Unknown,
    Authorized,
    Pending,
    Denied,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Reply {
    Ready {
        state: Authorization,
    },
    Response {
        key: String,
        generation: u64,
        action: String,
        value: String,
    },
    Error {
        #[serde(default)]
        key: Option<String>,
        message: String,
    },
}

pub fn delivered(reply: Reply) -> Option<Delivered> {
    let Reply::Response {
        key,
        generation,
        action,
        value,
    } = reply
    else {
        return None;
    };
    let response = match action.as_str() {
        "opened" => Response::Opened,
        "snoozed" => Response::Snoozed(value),
        _ => Response::Closed,
    };
    Some(Delivered {
        pr_key: key,
        generation,
        response,
    })
}

fn read_replies(
    stdout: ChildStdout,
    tx: Sender<Delivered>,
    authorization: Arc<Mutex<Authorization>>,
    reset: Arc<AtomicBool>,
) {
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        match serde_json::from_str::<Reply>(&line) {
            Ok(Reply::Ready { state }) => {
                let previous =
                    std::mem::replace(&mut *authorization.lock().expect("authorization lock poisoned"), state);
                if state == Authorization::Authorized
                    && matches!(previous, Authorization::Pending | Authorization::Denied)
                {
                    reset.store(true, Ordering::SeqCst);
                }
            }
            Ok(Reply::Error {
                key: Some(key),
                message,
            }) => warn!("notifier failed for {key}: {message}"),
            Ok(Reply::Error { key: None, message }) => warn!("notifier: {message}"),
            Ok(reply) => {
                if let Some(d) = delivered(reply) {
                    let _ = tx.send(d);
                }
            }
            Err(err) => warn!("unreadable notifier output {line:?}: {err}"),
        }
    }
}

pub struct NativeNotifier {
    program: PathBuf,
    actions: Vec<String>,
    tx: Sender<Delivered>,
    authorization: Arc<Mutex<Authorization>>,
    reset: Arc<AtomicBool>,
    child: Option<(Child, ChildStdin)>,
    spawned_at: Option<Instant>,
    quick_exits: u32,
    respawn_gap: Duration,
    fallback: Box<dyn Notifier>,
}

impl NativeNotifier {
    pub fn start(program: PathBuf, actions: Vec<String>, tx: Sender<Delivered>, fallback: Box<dyn Notifier>) -> Self {
        let mut notifier = NativeNotifier {
            program,
            actions,
            tx,
            authorization: Arc::new(Mutex::new(Authorization::Unknown)),
            reset: Arc::new(AtomicBool::new(false)),
            child: None,
            spawned_at: None,
            quick_exits: 0,
            respawn_gap: RESPAWN_GAP,
            fallback,
        };
        let _ = notifier.ensure_running();
        notifier
    }

    fn wait_before_respawn(&self) -> Duration {
        self.respawn_gap * 2u32.pow(self.quick_exits.saturating_sub(1).min(4))
    }

    fn authorization(&self) -> Authorization {
        *self.authorization.lock().expect("authorization lock poisoned")
    }

    fn ensure_running(&mut self) -> Result<()> {
        if let Some((child, _)) = &mut self.child
            && matches!(child.try_wait(), Ok(None))
        {
            return Ok(());
        }
        if let Some((mut child, _)) = self.child.take() {
            let _ = child.kill();
            let status = child.wait();
            let lived = self.spawned_at.map_or(Duration::MAX, |at| at.elapsed());
            self.quick_exits = if lived < STABLE_RUN { self.quick_exits + 1 } else { 0 };
            warn!("the notifier app exited ({status:?}) after {}s", lived.as_secs());
            self.clear_leftovers();
        }
        if let Some(at) = self.spawned_at
            && at.elapsed() < self.wait_before_respawn()
        {
            bail!("waiting before restarting the notifier app");
        }
        let restarting = self.spawned_at.is_some();
        self.spawned_at = Some(Instant::now());
        let mut command = Command::new(&self.program);
        command
            .arg(format!("--actions={}", self.actions.join(",")))
            .arg(format!("--mute-action={MUTE_TITLE}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                self.quick_exits += 1;
                warn!("starting {} failed: {err}", self.program.display());
                return Err(err).with_context(|| format!("starting {}", self.program.display()));
            }
        };
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let tx = self.tx.clone();
        let authorization = Arc::clone(&self.authorization);
        let reset = Arc::clone(&self.reset);
        std::thread::spawn(move || read_replies(stdout, tx, authorization, reset));
        self.child = Some((child, stdin));
        if restarting {
            self.reset.store(true, Ordering::SeqCst);
        }
        self.send(&Request::Clear)
    }

    fn clear_leftovers(&self) {
        let cleared = Command::new(&self.program)
            .arg("--clear")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status();
        if let Err(err) = cleared {
            warn!("clearing the notifications of the exited notifier app failed: {err}");
        }
    }

    fn send(&mut self, request: &Request<'_>) -> Result<()> {
        let (_, stdin) = self.child.as_mut().context("notifier is not running")?;
        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        stdin.write_all(line.as_bytes())?;
        stdin.flush()?;
        Ok(())
    }
}

impl Notifier for NativeNotifier {
    fn show(&mut self, n: &Notification) -> Result<()> {
        if matches!(self.authorization(), Authorization::Pending | Authorization::Denied)
            || self.ensure_running().is_err()
        {
            return self.fallback.show(n);
        }
        let request = Request::Show {
            key: &n.pr_key,
            generation: n.generation,
            title: &n.title,
            subtitle: &n.subtitle,
            message: &n.message,
            snoozable: n.snoozable,
        };
        if let Err(err) = self.send(&request) {
            warn!("the notifier app did not take {}: {err:#}", n.pr_key);
            return self.fallback.show(n);
        }
        Ok(())
    }

    fn remove(&mut self, pr_key: &str) -> Result<()> {
        self.ensure_running()?;
        self.send(&Request::Remove { key: pr_key })
    }

    fn degraded(&self) -> Option<String> {
        if self.child.is_none() {
            return Some(STOPPED.to_string());
        }
        match self.authorization() {
            Authorization::Denied => Some(DENIED.to_string()),
            Authorization::Pending => Some(PENDING.to_string()),
            Authorization::Unknown | Authorization::Authorized => None,
        }
    }

    fn take_reset(&mut self) -> bool {
        let _ = self.ensure_running();
        self.reset.swap(false, Ordering::SeqCst)
    }
}

impl Drop for NativeNotifier {
    fn drop(&mut self) {
        let Some((mut child, stdin)) = self.child.take() else {
            return;
        };
        drop(stdin);
        let deadline = Instant::now() + Duration::from_secs(2);
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::mpsc;

    use super::*;

    fn notification() -> Notification {
        Notification {
            pr_key: "acme/api#1".into(),
            generation: 2,
            title: "acme/api #1".into(),
            subtitle: "add rate limits".into(),
            message: "alice requested changes".into(),
            snoozable: true,
        }
    }

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<String>>>);

    impl Recorder {
        fn shown(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    impl Notifier for Recorder {
        fn show(&mut self, n: &Notification) -> Result<()> {
            self.0.lock().unwrap().push(n.pr_key.clone());
            Ok(())
        }

        fn remove(&mut self, _pr_key: &str) -> Result<()> {
            Ok(())
        }

        fn degraded(&self) -> Option<String> {
            None
        }
    }

    #[cfg(unix)]
    fn fake_helper(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("helper.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn logging_helper(dir: &Path, log: &Path, ready: &str) -> PathBuf {
        fake_helper(
            dir,
            &format!(
                r#"case "$1" in --clear) echo "leftovers cleared" >> "{log}"; exit 0;; esac
echo started >> "{log}"
echo '{{"event":"ready","state":"{ready}"}}'
while IFS= read -r line; do echo "$line" >> "{log}"; done"#,
                log = log.display()
            ),
        )
    }

    fn start(helper: PathBuf, actions: Vec<String>) -> (NativeNotifier, mpsc::Receiver<Delivered>, Recorder) {
        let (tx, rx) = mpsc::channel();
        let fallback = Recorder::default();
        let notifier = NativeNotifier::start(helper, actions, tx, Box::new(fallback.clone()));
        (notifier, rx, fallback)
    }

    fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn requests_serialize_as_json_lines() {
        let n = notification();
        let show = Request::Show {
            key: &n.pr_key,
            generation: 2,
            title: &n.title,
            subtitle: &n.subtitle,
            message: &n.message,
            snoozable: true,
        };
        assert_eq!(
            serde_json::to_string(&show).unwrap(),
            r#"{"op":"show","key":"acme/api#1","generation":2,"title":"acme/api #1","subtitle":"add rate limits","message":"alice requested changes","snoozable":true}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Remove { key: "k" }).unwrap(),
            r#"{"op":"remove","key":"k"}"#
        );
        assert_eq!(serde_json::to_string(&Request::Clear).unwrap(), r#"{"op":"clear"}"#);
    }

    #[test]
    fn replies_map_to_delivered_responses() {
        let parse = |s: &str| delivered(serde_json::from_str::<Reply>(s).unwrap());
        let d = parse(r#"{"event":"response","key":"k","generation":3,"action":"snoozed","value":"1h"}"#).unwrap();
        assert_eq!(
            (d.pr_key.as_str(), d.generation, d.response),
            ("k", 3, Response::Snoozed("1h".into()))
        );
        let opened = r#"{"event":"response","key":"k","generation":3,"action":"opened","value":"com.apple.UNNotificationDefaultActionIdentifier"}"#;
        assert_eq!(parse(opened).unwrap().response, Response::Opened);
        let closed = r#"{"event":"response","key":"k","generation":3,"action":"closed","value":"x"}"#;
        assert_eq!(parse(closed).unwrap().response, Response::Closed);
        assert_eq!(parse(r#"{"event":"ready","state":"authorized"}"#), None);
        assert_eq!(
            serde_json::from_str::<Reply>(r#"{"event":"error","key":"k","message":"boom"}"#).unwrap(),
            Reply::Error {
                key: Some("k".into()),
                message: "boom".into()
            }
        );
        assert!(serde_json::from_str::<Reply>(r#"{"event":"ready","state":"unknown"}"#).is_err());
    }

    #[test]
    fn the_swift_helper_reads_and_writes_the_same_fields() {
        let swift = include_str!("../../notifier/main.swift");
        let n = notification();
        let show = serde_json::to_value(Request::Show {
            key: &n.pr_key,
            generation: n.generation,
            title: &n.title,
            subtitle: &n.subtitle,
            message: &n.message,
            snoozable: n.snoozable,
        })
        .unwrap();
        for field in show.as_object().unwrap().keys().filter(|k| *k != "op") {
            assert!(
                swift.contains(&format!("command[\"{field}\"]")),
                "main.swift never reads {field}"
            );
        }
        for request in [
            serde_json::to_value(&show).unwrap(),
            serde_json::to_value(Request::Remove { key: "k" }).unwrap(),
            serde_json::to_value(Request::Clear).unwrap(),
        ] {
            let op = request["op"].as_str().unwrap();
            assert!(swift.contains(&format!("case \"{op}\":")), "main.swift ignores op {op}");
        }
        assert!(swift.contains(&format!("identifier: \"{}\"", super::super::MUTE_ACTION)));
        assert!(swift.contains("option(\"mute-action\")"));
        for needle in [
            r#""event": "ready", "state": state"#,
            r#"state = "authorized""#,
            r#"state = "pending""#,
            r#"state = "denied""#,
            r#""event": "response""#,
            r#""key": info["key"]"#,
            r#""generation": info["generation"]"#,
            r#""action": action"#,
            r#""value": response.actionIdentifier"#,
            r#"action = "opened""#,
            r#"action = "closed""#,
            r#"action = "snoozed""#,
            r#""event": "error", "key": key, "message""#,
        ] {
            assert!(swift.contains(needle), "main.swift lacks {needle}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn talks_to_one_long_lived_helper() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let helper = fake_helper(
            dir.path(),
            &format!(
                r#"echo "args:$*" >> "{log}"
echo '{{"event":"ready","state":"authorized"}}'
while IFS= read -r line; do
  echo "$line" >> "{log}"
  case "$line" in *'"op":"show"'*) echo '{{"event":"response","key":"acme/api#1","generation":2,"action":"snoozed","value":"1h"}}';; esac
done"#,
                log = log.display()
            ),
        );
        let (mut notifier, rx, fallback) = start(helper, vec!["15m".into(), "1h".into()]);
        notifier.show(&notification()).unwrap();
        notifier.remove("acme/api#1").unwrap();
        let d = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            (d.pr_key.as_str(), d.generation, d.response),
            ("acme/api#1", 2, Response::Snoozed("1h".into()))
        );
        assert!(wait_until(|| notifier.authorization() == Authorization::Authorized));
        assert_eq!(notifier.degraded(), None);
        assert!(!notifier.take_reset());
        drop(notifier);
        assert!(fallback.shown().is_empty());
        let lines: Vec<String> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        assert_eq!(lines[0], "args:--actions=15m,1h --mute-action=Mute everything today");
        assert_eq!(lines[1], r#"{"op":"clear"}"#);
        assert!(lines[2].starts_with(r#"{"op":"show","key":"acme/api#1","generation":2,"#));
        assert_eq!(lines[3], r#"{"op":"remove","key":"acme/api#1"}"#);
        assert_eq!(lines.len(), 4);
    }

    #[cfg(unix)]
    #[test]
    fn denied_permission_is_reported_and_shows_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let (mut notifier, _rx, fallback) = start(logging_helper(dir.path(), &log, "denied"), vec![]);
        assert!(wait_until(|| notifier.degraded().is_some()));
        assert_eq!(notifier.degraded().as_deref(), Some(DENIED));
        notifier.show(&notification()).unwrap();
        assert_eq!(fallback.shown(), vec!["acme/api#1".to_string()]);
        drop(notifier);
        assert!(!std::fs::read_to_string(&log).unwrap().contains(r#""op":"show""#));
    }

    #[cfg(unix)]
    #[test]
    fn granting_a_pending_permission_asks_for_a_resync() {
        let dir = tempfile::tempdir().unwrap();
        let helper = fake_helper(
            dir.path(),
            r#"echo '{"event":"ready","state":"pending"}'
while IFS= read -r line; do
  case "$line" in *'"op":"remove"'*) echo '{"event":"ready","state":"authorized"}';; esac
done"#,
        );
        let (mut notifier, _rx, fallback) = start(helper, vec![]);
        assert!(wait_until(|| notifier.degraded().as_deref() == Some(PENDING)));
        notifier.show(&notification()).unwrap();
        assert_eq!(fallback.shown().len(), 1);
        assert!(!notifier.take_reset());
        notifier.remove("acme/api#1").unwrap();
        assert!(wait_until(|| notifier.take_reset()));
        assert_eq!(notifier.degraded(), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_dead_helper_is_restarted_cleared_and_resynced() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let (mut notifier, _rx, _fallback) = start(logging_helper(dir.path(), &log, "authorized"), vec![]);
        notifier.respawn_gap = Duration::ZERO;
        assert!(!notifier.take_reset());
        assert!(wait_until(
            || std::fs::read_to_string(&log).is_ok_and(|t| t.contains("clear"))
        ));
        let (child, _) = notifier.child.as_mut().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(notifier.take_reset());
        assert!(!notifier.take_reset());
        notifier.show(&notification()).unwrap();
        drop(notifier);
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text.matches("started").count(), 2);
        assert_eq!(text.matches("leftovers cleared").count(), 1);
        assert_eq!(text.matches(r#"{"op":"clear"}"#).count(), 2);
        assert!(text.contains(r#""op":"show""#));
    }

    #[cfg(unix)]
    #[test]
    fn a_helper_that_dies_right_away_waits_before_restarting() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let (mut notifier, _rx, fallback) = start(logging_helper(dir.path(), &log, "authorized"), vec![]);
        assert!(wait_until(
            || std::fs::read_to_string(&log).is_ok_and(|t| t.contains("clear"))
        ));
        let (child, _) = notifier.child.as_mut().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!notifier.take_reset());
        assert_eq!(notifier.degraded().as_deref(), Some(STOPPED));
        assert!(std::fs::read_to_string(&log).unwrap().contains("leftovers cleared"));
        notifier.show(&notification()).unwrap();
        assert_eq!(fallback.shown(), vec!["acme/api#1".to_string()]);
        assert_eq!(notifier.quick_exits, 1);
        assert_eq!(notifier.wait_before_respawn(), RESPAWN_GAP);
        notifier.respawn_gap = Duration::ZERO;
        assert!(notifier.take_reset());
        assert_eq!(notifier.degraded(), None);
        drop(notifier);
        assert_eq!(std::fs::read_to_string(&log).unwrap().matches("started").count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn a_show_the_helper_cannot_take_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let helper = fake_helper(
            dir.path(),
            r#"exec 0<&-
echo '{"event":"ready","state":"authorized"}'
sleep 3"#,
        );
        let (mut notifier, _rx, fallback) = start(helper, vec![]);
        assert!(wait_until(|| notifier.authorization() == Authorization::Authorized));
        notifier.show(&notification()).unwrap();
        assert_eq!(fallback.shown(), vec!["acme/api#1".to_string()]);
    }

    #[test]
    fn repeated_quick_exits_back_off_up_to_eight_minutes() {
        let (tx, _rx) = mpsc::channel();
        let mut notifier = NativeNotifier::start(
            PathBuf::from("/nonexistent/gh-overview-notifier"),
            vec![],
            tx,
            Box::new(Recorder::default()),
        );
        assert_eq!(notifier.degraded().as_deref(), Some(STOPPED));
        let waits: Vec<u64> = (1..=6)
            .map(|n| {
                notifier.quick_exits = n;
                notifier.wait_before_respawn().as_secs()
            })
            .collect();
        assert_eq!(waits, vec![30, 60, 120, 240, 480, 480]);
    }

    #[test]
    fn an_app_that_cannot_start_falls_back_and_is_retried() {
        let (tx, _rx) = mpsc::channel();
        let fallback = Recorder::default();
        let mut notifier = NativeNotifier::start(
            PathBuf::from("/nonexistent/gh-overview-notifier"),
            vec![],
            tx,
            Box::new(fallback.clone()),
        );
        assert_eq!(notifier.quick_exits, 1);
        notifier.show(&notification()).unwrap();
        assert_eq!(fallback.shown().len(), 1);
        notifier.respawn_gap = Duration::ZERO;
        assert!(!notifier.take_reset());
        assert_eq!(notifier.quick_exits, 2);
        assert_eq!(notifier.degraded().as_deref(), Some(STOPPED));
    }
}
