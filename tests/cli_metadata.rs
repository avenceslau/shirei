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
fn capabilities_json_lists_commands_and_flags_from_the_parser() {
    let capabilities: serde_json::Value =
        serde_json::from_str(&shirei(&["capabilities", "--json"])).unwrap();
    assert_eq!(capabilities["version"], env!("CARGO_PKG_VERSION"));
    assert!(capabilities["git_sha"].is_string());
    let commands = capabilities["commands"].as_array().unwrap();
    let command = |name: &str| {
        commands
            .iter()
            .find(|command| command["command"] == name)
            .unwrap_or_else(|| panic!("missing {name}"))
    };
    let flags = |name: &str| -> Vec<String> {
        command(name)["flags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|flag| flag["long"].as_str().unwrap().to_string())
            .collect()
    };

    for flag in [
        "--base",
        "--no-open",
        "--open",
        "--print-path",
        "--sparse",
        "--install",
    ] {
        assert!(flags("shirei branch").contains(&flag.to_string()), "{flag}");
        assert!(
            flags("shirei stack branch").contains(&flag.to_string()),
            "{flag}"
        );
    }
    assert_eq!(
        command("shirei branch")["positionals"],
        serde_json::json!(["name"])
    );
    let install = command("shirei branch")["flags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|flag| flag["long"] == "--install")
        .unwrap();
    assert_eq!(
        install["values"],
        serde_json::json!(["sync", "background", "none"])
    );
    assert!(flags("shirei pr create").contains(&"--remote".to_string()));
    assert!(!flags("shirei pr create").contains(&"--expected-head".to_string()));
    assert!(flags("shirei worktrees prune").contains(&"--merged".to_string()));
    command("shirei worktrees add");
    command("shirei capabilities");
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
