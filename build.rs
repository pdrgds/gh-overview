use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=notifier/main.swift");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => panic!("unsupported macOS architecture {other:?}"),
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("gh-overview-notifier");
    let status = Command::new("swiftc")
        .args([
            "-swift-version",
            "5",
            "-target",
            &format!("{arch}-apple-macos13.0"),
            "-O",
            "-o",
        ])
        .arg(&out)
        .arg("notifier/main.swift")
        .status()
        .expect("swiftc is required to build gh-overview on macOS; install Xcode or the Command Line Tools");
    assert!(status.success(), "swiftc failed to compile notifier/main.swift");
}
