use std::{env, error::Error, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=native/AppleBridge.swift");
    println!("cargo:rerun-if-changed=native/Info.plist");
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return Ok(());
    }
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let out = PathBuf::from(env::var("OUT_DIR")?).join("lightcraft-apple");
    let arch = match env::var("CARGO_CFG_TARGET_ARCH")?.as_str() {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        other => return Err(format!("unsupported Apple bridge architecture: {other}").into()),
    };
    let version = env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_else(|_| "11.0".into());
    let result = Command::new("xcrun")
        .args(["swiftc", "-O", "-swift-version", "5", "-parse-as-library", "-target"])
        .arg(format!("{arch}-apple-macosx{version}"))
        .arg(root.join("native/AppleBridge.swift"))
        .args(["-Xlinker", "-sectcreate", "-Xlinker", "__TEXT", "-Xlinker", "__info_plist", "-Xlinker"])
        .arg(root.join("native/Info.plist"))
        .arg("-o")
        .arg(&out)
        .output()?;
    if !result.status.success() {
        return Err(format!("compiling the Apple bridge: {}", String::from_utf8_lossy(result.stderr.get(..16_000).unwrap_or(&result.stderr))).into());
    }
    println!("cargo:rustc-env=LIGHTCRAFT_APPLE_HELPER={}", out.display());
    Ok(())
}
