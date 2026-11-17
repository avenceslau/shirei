use std::path::{Path, PathBuf};
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    // A crates.io build can sit inside an unrelated repository; only use the
    // SHA when this package is the repository's root.
    let toplevel = git(&["rev-parse", "--show-toplevel"]).map(PathBuf::from);
    let is_package_repo = toplevel
        .and_then(|path| path.canonicalize().ok())
        .zip(manifest_dir.canonicalize().ok())
        .is_some_and(|(toplevel, manifest)| toplevel == manifest);
    let sha = is_package_repo
        .then(|| git(&["rev-parse", "--short", "HEAD"]))
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=SHIREI_GIT_SHA={sha}");
    if !is_package_repo {
        return;
    }

    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]) {
        println!(
            "cargo:rerun-if-changed={}",
            Path::new(&git_dir).join("HEAD").display()
        );
    }
    if let Some(common_dir) = git(&["rev-parse", "--git-common-dir"]) {
        let common_dir = Path::new(&common_dir);
        println!(
            "cargo:rerun-if-changed={}",
            common_dir.join("packed-refs").display()
        );
        if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
            println!(
                "cargo:rerun-if-changed={}",
                common_dir.join(reference).display()
            );
        }
    }
}
