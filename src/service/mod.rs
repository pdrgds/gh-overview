pub mod launchd;

use std::path::Path;

pub trait ServiceInstaller {
    fn install(&self, exe: &Path, log: &Path, env: &[(String, String)]) -> anyhow::Result<()>;
    fn uninstall(&self) -> anyhow::Result<()>;
}
