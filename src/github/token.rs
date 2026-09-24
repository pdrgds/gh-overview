use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use super::FetchError;

const GH_TIMEOUT: Duration = Duration::from_secs(10);

pub trait TokenProvider {
    fn token(&self, login: &str) -> Result<String, FetchError>;
}

pub struct GhCli;

pub fn output_within(mut command: Command, timeout: Duration) -> std::io::Result<Option<Output>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        out.read_to_end(&mut stdout)?;
    }
    if let Some(mut err) = child.stderr.take() {
        err.read_to_end(&mut stderr)?;
    }
    Ok(Some(Output { status, stdout, stderr }))
}

impl TokenProvider for GhCli {
    fn token(&self, login: &str) -> Result<String, FetchError> {
        let mut command = Command::new("gh");
        command.args(["auth", "token", "--user", login]);
        let out = output_within(command, GH_TIMEOUT)
            .map_err(|e| FetchError::Token(e.to_string()))?
            .ok_or_else(|| FetchError::Token(format!("`gh auth token --user {login}` timed out")))?;
        if !out.status.success() {
            return Err(FetchError::Token(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if token.is_empty() {
            return Err(FetchError::Token(format!("empty token for {login}")));
        }
        Ok(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_of_a_fast_command() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo token-123; echo oops >&2"]);
        let out = output_within(command, Duration::from_secs(5)).unwrap().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "token-123");
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "oops");
    }

    #[test]
    fn kills_a_command_that_exceeds_the_timeout() {
        let mut command = Command::new("sleep");
        command.arg("5");
        let started = Instant::now();
        assert!(output_within(command, Duration::from_millis(100)).unwrap().is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
