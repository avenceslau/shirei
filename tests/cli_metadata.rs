use std::process::Command;

fn shirei(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_shirei"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn version_includes_the_git_sha() {
    let version = shirei(&["--version"]);
    let expected_prefix = format!("shirei {} (", env!("CARGO_PKG_VERSION"));
    let sha = version
        .trim()
        .strip_prefix(&expected_prefix)
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or_else(|| panic!("unexpected version: {version}"));
    assert!(
        sha == "unknown" || (sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit())),
        "{version}"
    );
}
