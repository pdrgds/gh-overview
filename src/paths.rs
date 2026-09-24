use std::path::PathBuf;

use anyhow::Result;
use etcetera::BaseStrategy;

const APP: &str = "gh-overview";

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_file: PathBuf,
    pub db_file: PathBuf,
    pub log_file: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self> {
        let base = etcetera::choose_base_strategy()?;
        let state = base.state_dir().unwrap_or_else(|| base.data_dir());
        Ok(Paths {
            config_file: base.config_dir().join(APP).join("config.toml"),
            db_file: base.data_dir().join(APP).join("state.db"),
            log_file: state.join(APP).join("daemon.log"),
        })
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        for file in [&self.config_file, &self.db_file, &self.log_file] {
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
                if let Err(err) = restrict_to_owner(dir) {
                    eprintln!("warning: could not make {} private: {err}", dir.display());
                }
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn restrict_to_owner(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_to_owner(_dir: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn app_directories_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_file: root.path().join("config/gh-overview/config.toml"),
            db_file: root.path().join("data/gh-overview/state.db"),
            log_file: root.path().join("state/gh-overview/daemon.log"),
        };
        paths.ensure_dirs().unwrap();
        for dir in ["config/gh-overview", "data/gh-overview", "state/gh-overview"] {
            let mode = std::fs::metadata(root.path().join(dir)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{dir}");
        }
    }
}
