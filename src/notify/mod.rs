pub mod bundle;
pub mod native;
pub mod osascript;

pub const MUTE_ACTION: &str = "mute-today";
pub const MUTE_TITLE: &str = "Mute everything today";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub pr_key: String,
    pub generation: u64,
    pub title: String,
    pub subtitle: String,
    pub message: String,
    pub snoozable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Opened,
    Snoozed(String),
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivered {
    pub pr_key: String,
    pub generation: u64,
    pub response: Response,
}

pub trait Notifier {
    fn show(&mut self, notification: &Notification) -> anyhow::Result<()>;
    fn remove(&mut self, pr_key: &str) -> anyhow::Result<()>;
    fn degraded(&self) -> Option<String>;

    fn take_reset(&mut self) -> bool {
        false
    }
}

#[cfg(unix)]
pub(crate) fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
pub(crate) fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn only_executable_files_count() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        let tool = dir.path().join("tool");
        std::fs::write(&plain, "").unwrap();
        std::fs::write(&tool, "").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!is_executable(&plain));
        assert!(is_executable(&tool));
        assert!(!is_executable(dir.path()));
    }
}
