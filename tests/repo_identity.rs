#![cfg(unix)]

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Repo {
    _temp: TempDir,
    root: PathBuf,
    main: PathBuf,
    linked: PathBuf,
    state_dir: PathBuf,
    config: PathBuf,
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A main checkout at `<root>/repo` and a linked worktree at `<root>/aaa-linked`,
/// which sorts before the main checkout the way `~/.local/...` sorts before
/// `~/projects/...`.
fn setup() -> Repo {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let main = root.join("repo");
    let linked = root.join("aaa-linked");
    let state_dir = root.join("state");
    let config = root.join("config.toml");
    fs::create_dir(&main).unwrap();
    fs::create_dir(&state_dir).unwrap();
    fs::write(
        &config,
        format!(
            "roots = []\nworktree_root = \"{}\"\nrun_install_on_create = false\n",
            root.join("worktrees").display()
        ),
    )
    .unwrap();

    git(&main, &["init", "--initial-branch=main"]);
    git(&main, &["config", "user.email", "test@example.com"]);
    git(&main, &["config", "user.name", "Test User"]);
    fs::write(main.join("README.md"), "main\n").unwrap();
    git(&main, &["add", "README.md"]);
    git(&main, &["commit", "-m", "initial"]);
    let linked_arg = linked.to_string_lossy().to_string();
    git(
        &main,
        &["worktree", "add", "-b", "feat/linked", &linked_arg],
    );
    git(&main, &["branch", "feat/from-linked"]);
    git(&main, &["branch", "feat/from-main"]);

    Repo {
        _temp: temp,
        root,
        main,
        linked,
        state_dir,
        config,
    }
}

fn run_shirei(repo: &Repo, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_shirei"))
        .args(args)
        .current_dir(cwd)
        .env("SHIREI_STATE_DIR", &repo.state_dir)
        .env("SHIREI_CONFIG", &repo.config)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn read_state(repo: &Repo) -> Value {
    serde_json::from_slice(&fs::read(repo.state_dir.join("state.json")).unwrap()).unwrap()
}

#[test]
fn stack_metadata_is_shared_between_the_main_checkout_and_linked_worktrees() {
    let repo = setup();

    assert_success(&run_shirei(
        &repo,
        &repo.linked,
        &["stack", "track", "feat/from-linked", "--base", "main"],
    ));
    assert_success(&run_shirei(
        &repo,
        &repo.main,
        &["stack", "track", "feat/from-main", "--base", "main"],
    ));

    for cwd in [&repo.main, &repo.linked] {
        let listed = assert_success(&run_shirei(&repo, cwd, &["stack", "list"]));
        assert!(listed.contains("feat/from-linked"), "{listed}");
        assert!(listed.contains("feat/from-main"), "{listed}");
        assert!(listed.contains(&format!("repo={}", repo.main.display())));
    }

    let state = read_state(&repo);
    for item in state["stack_branches"].as_array().unwrap() {
        assert_eq!(item["repo"], json!(repo.main));
    }
}

#[test]
fn state_keyed_by_a_linked_worktree_is_migrated_to_the_main_checkout() {
    let repo = setup();
    let timestamp = "2026-01-01T00:00:00Z";
    let state = json!({
        "recent": [],
        "worktrees": [
            {"path": repo.linked, "repo": repo.linked, "branch": "feat/linked", "created": timestamp}
        ],
        "stack_branches": [
            {"repo": repo.linked, "branch": "feat/linked", "base": "main", "created": timestamp, "updated": timestamp}
        ]
    });
    fs::write(
        repo.state_dir.join("state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();

    let listed = assert_success(&run_shirei(&repo, &repo.main, &["stack", "list"]));
    assert!(listed.contains("feat/linked"), "{listed}");
    assert!(!listed.contains("missing-worktree"), "{listed}");

    let migrated = read_state(&repo);
    assert_eq!(migrated["repos_keyed_by_main_worktree"], json!(true));
    assert_eq!(migrated["worktrees"][0]["repo"], json!(repo.main));
    assert_eq!(migrated["stack_branches"][0]["repo"], json!(repo.main));
}

#[test]
fn scan_files_every_worktree_under_the_main_checkout() {
    let repo = setup();
    let second = repo.root.join("worktrees").join("second");
    let second_arg = second.to_string_lossy().to_string();
    git(
        &repo.main,
        &["worktree", "add", "-b", "feat/second", &second_arg],
    );
    fs::write(
        &repo.config,
        format!(
            "roots = [\"{}/*\"]\nrun_install_on_create = false\n",
            repo.root.display()
        ),
    )
    .unwrap();
    let timestamp = "2026-01-01T00:00:00Z";
    let state = json!({
        "recent": [],
        "repos_keyed_by_main_worktree": true,
        "worktrees": [
            {"path": second, "repo": repo.linked, "branch": "feat/second", "created": timestamp}
        ]
    });
    fs::write(
        repo.state_dir.join("state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();

    assert_success(&run_shirei(&repo, &repo.main, &["worktrees", "scan"]));

    let scanned = read_state(&repo);
    let worktrees = scanned["worktrees"].as_array().unwrap();
    assert_eq!(worktrees.len(), 2, "{worktrees:?}");
    for worktree in worktrees {
        assert_eq!(worktree["repo"], json!(repo.main), "{worktree}");
    }
}
