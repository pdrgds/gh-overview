use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{error, info, warn};

use super::Daemon;
use crate::browser;
use crate::clock::SystemClock;
use crate::config::Config;
use crate::github::client::GithubClient;
use crate::github::token::GhCli;
use crate::notify::Notifier;
use crate::notify::bundle;
use crate::notify::native::NativeNotifier;
use crate::notify::osascript::OsascriptNotifier;
use crate::paths::Paths;
use crate::store::Store;

const TICK: Duration = Duration::from_secs(5);
const COMMAND_CHECK: Duration = Duration::from_millis(250);
const LOG_MAX_BYTES: u64 = 5_000_000;
const LOG_KEEP_BYTES: u64 = 1_000_000;

pub fn trim_log(path: &Path, max_bytes: u64, keep_bytes: u64) -> std::io::Result<()> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if meta.len() <= max_bytes {
        return Ok(());
    }
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(meta.len() - keep_bytes.min(meta.len())))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    let start = tail.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1);
    std::fs::write(path, &tail[start..])
}

fn lock_single_instance(path: &Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => bail!("another gh-overview daemon is already running"),
        Err(std::fs::TryLockError::Error(err)) => Err(err).with_context(|| format!("locking {}", path.display())),
    }
}

pub fn run(paths: &Paths, config: Config) -> Result<()> {
    if let Err(err) = trim_log(&paths.log_file, LOG_MAX_BYTES, LOG_KEEP_BYTES) {
        warn!("trimming {} failed: {err}", paths.log_file.display());
    }
    let _lock = lock_single_instance(&paths.db_file.with_file_name("daemon.lock"))?;
    let store = Store::open(&paths.db_file)?;
    let (tx, rx) = mpsc::channel();
    let labels = config.snooze().iter().map(|c| c.label(config.tomorrow_hour)).collect();
    let notifier: Box<dyn Notifier> = match bundle::installed_executable() {
        Some(program) => Box::new(NativeNotifier::start(
            program,
            labels,
            tx.clone(),
            Box::new(OsascriptNotifier::fallback()),
        )),
        None => {
            let reason = "the gh-overview notifier app is not installed; run `ghov install`";
            warn!("{reason}; falling back to osascript");
            Box::new(OsascriptNotifier::because(reason))
        }
    };
    let github = Box::new(GithubClient::new(GhCli)?);
    let accounts = config.clone();
    let opener = Box::new(move |account: &str, url: &str| {
        if let Err(err) = browser::open_url(accounts.browser_for(account), url) {
            warn!("opening {url} failed: {err:#}");
        }
    });
    let mut daemon = Daemon::new(store, github, notifier, Box::new(SystemClock), opener, config);
    if let Err(err) = daemon.prune_unknown_accounts() {
        warn!("pruning rows of removed accounts failed: {err:#}");
    }
    info!("daemon started");
    loop {
        if let Err(err) = daemon.cycle() {
            error!("cycle failed: {err:#}");
        }
        let next_cycle = Instant::now() + TICK;
        while let Some(left) = next_cycle.checked_duration_since(Instant::now()) {
            let wait = left.min(COMMAND_CHECK);
            match rx.recv_timeout(wait) {
                Ok(delivered) => {
                    for d in std::iter::once(delivered).chain(rx.try_iter()) {
                        if let Err(err) = daemon.handle(d) {
                            error!("handling notification response failed: {err:#}");
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => std::thread::sleep(wait),
            }
            if let Err(err) = daemon.consume_commands() {
                error!("applying TUI commands failed: {err:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_daemon_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");
        let held = lock_single_instance(&path).unwrap();
        let err = lock_single_instance(&path).unwrap_err();
        assert!(err.to_string().contains("already running"));
        drop(held);
        lock_single_instance(&path).unwrap();
    }

    #[test]
    fn small_logs_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        trim_log(&path, 100, 10).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        trim_log(&dir.path().join("missing.log"), 100, 10).unwrap();
    }

    #[test]
    fn large_logs_keep_whole_lines_from_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        let text: String = (0..100).map(|i| format!("line {i:03}\n")).collect();
        std::fs::write(&path, &text).unwrap();
        trim_log(&path, 500, 25).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "line 098\nline 099\n");
    }
}
