#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::tempdir;

fn git(path: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {} failed", args.join(" "));
}

fn run_shirei(
    cwd: &Path,
    config: &Path,
    state_dir: &Path,
    bin_dir: &Path,
    log: &Path,
    args: &[&str],
) -> Output {
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(env!("CARGO_BIN_EXE_shirei"))
        .current_dir(cwd)
        .args(args)
        .env("SHIREI_CONFIG", config)
        .env("SHIREI_STATE_DIR", state_dir)
        .env("HERDR_ENV", "1")
        .env("HERDR_LOG", log)
        .env("PATH", path)
        .output()
        .unwrap()
}

#[test]
fn opens_linked_checkouts_as_herdr_worktrees_from_the_primary_repo() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree_root = temp.path().join("worktrees");
    let state_dir = temp.path().join("state");
    let bin_dir = temp.path().join("bin");
    let config = temp.path().join("config.toml");
    let log = temp.path().join("herdr.log");

    fs::create_dir(&repo).unwrap();
    fs::create_dir(&bin_dir).unwrap();
    git(&repo, &["init", "--initial-branch=main"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    fs::write(repo.join("README.md"), "main\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "initial"]);

    fs::write(
        &config,
        format!(
            "backend = \"herdr\"\nworktree_root = \"{}\"\nrun_install_on_create = false\n",
            worktree_root.display()
        ),
    )
    .unwrap();

    let herdr = bin_dir.join("herdr");
    fs::write(
        &herdr,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$HERDR_LOG"
case "$1 $2" in
  "api snapshot")
    printf '%s\n' '{"result":{"snapshot":{"workspaces":[],"panes":[]}}}'
    ;;
  "worktree open")
    printf '%s\n' '{"result":{"workspace":{"workspace_id":"w-worktree"}}}'
    ;;
  "workspace focus")
    ;;
  *)
    printf 'unexpected herdr command: %s\n' "$*" >&2
    exit 1
    ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&herdr, fs::Permissions::from_mode(0o755)).unwrap();

    let fzf = bin_dir.join("fzf");
    fs::write(&fzf, "#!/bin/sh\ngrep '/repo/feature-one' | head -1\n").unwrap();
    fs::set_permissions(&fzf, fs::Permissions::from_mode(0o755)).unwrap();

    let first = run_shirei(
        &repo,
        &config,
        &state_dir,
        &bin_dir,
        &log,
        &["branch", "feature/one", "--base", "main"],
    );
    assert!(
        first.status.success(),
        "shirei failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let first_worktree = worktree_root.join("repo").join("feature-one");
    let second = run_shirei(
        &first_worktree,
        &config,
        &state_dir,
        &bin_dir,
        &log,
        &["stack", "branch", "feature/two", "--base", "feature/one"],
    );
    assert!(
        second.status.success(),
        "shirei failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let listed = run_shirei(
        &first_worktree,
        &config,
        &state_dir,
        &bin_dir,
        &log,
        &["stack", "list"],
    );
    assert!(
        listed.status.success(),
        "shirei failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert!(String::from_utf8_lossy(&listed.stdout).contains("feature/two"));

    let reopened = run_shirei(&repo, &config, &state_dir, &bin_dir, &log, &["pick"]);
    assert!(
        reopened.status.success(),
        "shirei failed: {}",
        String::from_utf8_lossy(&reopened.stderr)
    );

    let commands = fs::read_to_string(&log).unwrap();
    let primary_repo = repo.canonicalize().unwrap();
    let expected_first = format!(
        "worktree open --cwd {} --path {} --label repo-feature-one --no-focus",
        primary_repo.display(),
        first_worktree.display()
    );
    let expected_second = format!(
        "worktree open --cwd {} --path {} --label feature-one-feature-two --no-focus",
        primary_repo.display(),
        worktree_root
            .join("feature-one")
            .join("feature-two")
            .display()
    );
    assert_eq!(
        commands
            .lines()
            .filter(|line| *line == expected_first)
            .count(),
        2,
        "expected branch creation and picker reopen in:\n{commands}"
    );
    assert!(
        commands.lines().any(|line| line == expected_second),
        "missing `{expected_second}` in:\n{commands}"
    );
    assert!(!commands.contains("workspace create"));

    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(state_dir.join("state.json")).unwrap()).unwrap();
    let stack_repo = first_worktree.canonicalize().unwrap();
    assert_eq!(
        state["stack_branches"][0]["repo"].as_str(),
        Some(stack_repo.to_str().unwrap())
    );
}
