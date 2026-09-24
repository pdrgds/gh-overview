use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::is_executable;

pub const APP_NAME: &str = "GhOverview Notifier.app";
pub const EXECUTABLE: &str = "gh-overview-notifier";
pub const BUNDLE_ID: &str = "dev.pdrgds.gh-overview.notifier";

#[cfg(target_os = "macos")]
pub const NOTIFIER_BINARY: Option<&[u8]> = Some(include_bytes!(concat!(env!("OUT_DIR"), "/gh-overview-notifier")));

#[cfg(not(target_os = "macos"))]
pub const NOTIFIER_BINARY: Option<&[u8]> = None;

pub fn app_path(home: &Path) -> PathBuf {
    home.join("Applications").join(APP_NAME)
}

pub fn executable_path(app: &Path) -> PathBuf {
    app.join("Contents/MacOS").join(EXECUTABLE)
}

pub fn current_user_app() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(app_path(Path::new(&home)))
}

pub fn installed_executable() -> Option<PathBuf> {
    let executable = executable_path(&current_user_app().ok()?);
    is_executable(&executable).then_some(executable)
}

pub fn info_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>
  <string>{BUNDLE_ID}</string>
  <key>CFBundleName</key>
  <string>gh-overview</string>
  <key>CFBundleDisplayName</key>
  <string>gh-overview</string>
  <key>CFBundleExecutable</key>
  <string>{EXECUTABLE}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>{version}</string>
  <key>CFBundleVersion</key>
  <string>{version}</string>
  <key>LSMinimumSystemVersion</key>
  <string>13.0</string>
  <key>LSUIElement</key>
  <true/>
</dict>
</plist>
"#,
        version = env!("CARGO_PKG_VERSION")
    )
}

pub fn write_bundle(app: &Path, binary: &[u8]) -> Result<()> {
    let executable = executable_path(app);
    std::fs::create_dir_all(executable.parent().expect("executable lives in Contents/MacOS"))?;
    replace_file(&app.join("Contents/Info.plist"), info_plist().as_bytes(), false)?;
    replace_file(&executable, binary, true)?;
    Ok(())
}

fn replace_file(path: &Path, contents: &[u8], executable: bool) -> Result<()> {
    let name = path.file_name().expect("bundle files have names").to_string_lossy();
    let staged = path.with_file_name(format!(".{name}.new"));
    std::fs::write(&staged, contents).with_context(|| format!("writing {}", staged.display()))?;
    if executable {
        make_executable(&staged)?;
    }
    std::fs::rename(&staged, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

pub fn sign(app: &Path) -> Result<()> {
    let out = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(app)
        .output()
        .context("running codesign")?;
    if !out.status.success() {
        bail!("codesign failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

pub fn request_permission(app: &Path) -> Result<()> {
    let status = Command::new("open")
        .args(["-n", "-g"])
        .arg(app)
        .args(["--args", "--request-permission"])
        .status()
        .context("running open")?;
    if !status.success() {
        bail!("open {} failed ({status})", app.display());
    }
    Ok(())
}

pub fn clear_and_remove(app: &Path) -> Result<()> {
    let executable = executable_path(app);
    if is_executable(&executable) {
        let _ = Command::new(&executable).arg("--clear").status();
    }
    if app.exists() {
        std::fs::remove_dir_all(app)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_lives_in_the_users_applications_folder() {
        let app = app_path(Path::new("/Users/me"));
        assert_eq!(app, Path::new("/Users/me/Applications/GhOverview Notifier.app"));
        assert_eq!(
            executable_path(&app),
            Path::new("/Users/me/Applications/GhOverview Notifier.app/Contents/MacOS/gh-overview-notifier")
        );
    }

    #[test]
    fn info_plist_identifies_a_background_app() {
        let plist = info_plist();
        assert!(plist.contains("<string>dev.pdrgds.gh-overview.notifier</string>"));
        assert!(plist.contains("<key>CFBundleExecutable</key>\n  <string>gh-overview-notifier</string>"));
        assert!(plist.contains("<key>LSUIElement</key>\n  <true/>"));
    }

    #[cfg(unix)]
    #[test]
    fn writes_an_executable_bundle_and_removes_it() {
        use std::os::unix::fs::MetadataExt;
        let home = tempfile::tempdir().unwrap();
        let app = app_path(home.path());
        write_bundle(&app, b"old").unwrap();
        let first = std::fs::metadata(executable_path(&app)).unwrap();
        write_bundle(&app, b"binary").unwrap();
        assert_eq!(std::fs::read(executable_path(&app)).unwrap(), b"binary");
        assert!(is_executable(&executable_path(&app)));
        assert!(!is_executable(&app.join("Contents/Info.plist")));
        assert_ne!(std::fs::metadata(executable_path(&app)).unwrap().ino(), first.ino());
        assert_eq!(std::fs::read_dir(app.join("Contents/MacOS")).unwrap().count(), 1);
        assert!(
            std::fs::read_to_string(app.join("Contents/Info.plist"))
                .unwrap()
                .contains(BUNDLE_ID)
        );
        std::fs::remove_file(executable_path(&app)).unwrap();
        clear_and_remove(&app).unwrap();
        assert!(!app.exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_notifier_binary_is_embedded() {
        let binary = NOTIFIER_BINARY.expect("macOS builds embed the notifier");
        assert_eq!(&binary[..4], &[0xcf, 0xfa, 0xed, 0xfe]);
    }
}
