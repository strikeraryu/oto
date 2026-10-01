use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=native/Audio.swift");
    println!("cargo:rerun-if-changed=native/Info.plist");
    if env::var("CARGO_CFG_TARGET_OS").unwrap() != "macos" {
        return;
    }
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => panic!("Unsupported macOS architecture: {other}"),
    };
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("oto-audio");
    let plist = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("native/Info.plist");
    let status = Command::new("xcrun")
        .args(["swiftc", "-swift-version", "5", "-O", "-target"])
        .arg(format!("{arch}-apple-macosx14.2"))
        .args(["native/Audio.swift", "-o"])
        .arg(&output)
        .args([
            "-Xlinker",
            "-sectcreate",
            "-Xlinker",
            "__TEXT",
            "-Xlinker",
            "__info_plist",
            "-Xlinker",
        ])
        .arg(plist)
        .status()
        .expect("Install Xcode Command Line Tools with xcode-select --install");
    assert!(status.success(), "Could not compile the macOS audio engine");
    let status = Command::new("codesign")
        .args(["--force", "--sign", "-", "--identifier", "audio.oto.engine"])
        .arg(&output)
        .status()
        .expect("codesign is required on macOS");
    assert!(status.success(), "Could not sign the macOS audio engine");
}
