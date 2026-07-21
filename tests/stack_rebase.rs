#![cfg(unix)]

use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct StackRepo {
    _temp: TempDir,
    repo: PathBuf,
    worktrees: Vec<PathBuf>,
    state_dir: PathBuf,
    bin_dir: PathBuf,
    rebase_log: PathBuf,
    main_before: String,
    main_after: String,
    heads_before: Vec<String>,
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

fn commit_file(worktree: &Path, file: &str, content: &str, subject: &str) {
    fs::write(worktree.join(file), content).unwrap();
    git(worktree, &["add", file]);
    git(worktree, &["commit", "-m", subject]);
}

fn executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn setup_stack(root_conflict: bool, child_conflict: bool) -> StackRepo {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree_a = temp.path().join("worktree-a");
    let worktree_b = temp.path().join("worktree-b");
    let worktree_c = temp.path().join("worktree-c");
    let state_dir = temp.path().join("state");
    let bin_dir = temp.path().join("bin");
    let rebase_log = temp.path().join("rebase.log");
    fs::create_dir(&repo).unwrap();
    fs::create_dir(&state_dir).unwrap();
    fs::create_dir(&bin_dir).unwrap();

    git(&repo, &["init", "--initial-branch=main"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    commit_file(&repo, "README.md", "root\n", "root");
    let main_before = git(&repo, &["rev-parse", "HEAD"]);

    let worktree_a_arg = worktree_a.to_string_lossy().to_string();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "feat/a",
            &worktree_a_arg,
            &main_before,
        ],
    );
    if root_conflict {
        commit_file(&worktree_a, "README.md", "feature\n", "A");
    } else {
        commit_file(&worktree_a, "a.txt", "a\n", "A");
    }
    let head_a = git(&worktree_a, &["rev-parse", "HEAD"]);

    let worktree_b_arg = worktree_b.to_string_lossy().to_string();
    git(
        &repo,
        &["worktree", "add", "-b", "feat/b", &worktree_b_arg, "feat/a"],
    );
    if child_conflict {
        commit_file(&worktree_b, "README.md", "child\n", "B");
    } else {
        commit_file(&worktree_b, "b.txt", "b\n", "B");
    }
    let head_b = git(&worktree_b, &["rev-parse", "HEAD"]);

    let worktree_c_arg = worktree_c.to_string_lossy().to_string();
    git(
        &repo,
        &["worktree", "add", "-b", "feat/c", &worktree_c_arg, "feat/b"],
    );
    commit_file(&worktree_c, "c.txt", "c\n", "C");
    let head_c = git(&worktree_c, &["rev-parse", "HEAD"]);

    if root_conflict {
        commit_file(&repo, "README.md", "main\n", "main advances");
    } else {
        commit_file(&repo, "main.txt", "main\n", "main advances");
    }
    let main_after = git(&repo, &["rev-parse", "HEAD"]);

    let canonical_repo = fs::canonicalize(&repo).unwrap();
    let worktrees = [worktree_a, worktree_b, worktree_c]
        .into_iter()
        .map(|path| fs::canonicalize(path).unwrap())
        .collect::<Vec<_>>();
    let timestamp = "2026-01-01T00:00:00Z";
    let state = json!({
        "recent": [],
        "worktrees": [
            {"path": worktrees[0], "repo": canonical_repo, "branch": "feat/a", "created": timestamp},
            {"path": worktrees[1], "repo": canonical_repo, "branch": "feat/b", "created": timestamp},
            {"path": worktrees[2], "repo": canonical_repo, "branch": "feat/c", "created": timestamp}
        ],
        "stack_branches": [
            {"repo": canonical_repo, "branch": "feat/a", "base": "main", "created": timestamp, "updated": timestamp},
            {"repo": canonical_repo, "branch": "feat/b", "base": "feat/a", "created": timestamp, "updated": timestamp},
            {"repo": canonical_repo, "branch": "feat/c", "base": "feat/b", "created": timestamp, "updated": timestamp}
        ]
    });
    fs::write(
        state_dir.join("state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();

    let git_path = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    executable(
        &bin_dir.join("git"),
        &format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = rebase ]; then printf '%s\\n' \"$*\" >> \"$SHIREI_REBASE_LOG\"; fi\ndone\nexec \"{}\" \"$@\"\n",
            git_path.trim()
        ),
    );

    StackRepo {
        _temp: temp,
        repo: canonical_repo,
        worktrees,
        state_dir,
        bin_dir,
        rebase_log,
        main_before,
        main_after,
        heads_before: vec![head_a, head_b, head_c],
    }
}

fn run_shirei(stack: &StackRepo, args: &[&str]) -> Output {
    let path = format!(
        "{}:{}",
        stack.bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(env!("CARGO_BIN_EXE_shirei"))
        .args(args)
        .current_dir(&stack.repo)
        .env("PATH", path)
        .env("SHIREI_STATE_DIR", &stack.state_dir)
        .env("SHIREI_REBASE_LOG", &stack.rebase_log)
        .output()
        .unwrap()
}

#[test]
fn cascade_rebase_uses_each_worktree_and_exact_oid_boundaries() {
    let stack = setup_stack(false, false);
    let repo_arg = stack.repo.to_string_lossy().to_string();
    let args = ["stack", "rebase", "--branch", "feat/c", "--repo", &repo_arg];

    let dry_run = run_shirei(&stack, &[&args[..], &["--dry-run"]].concat());
    assert!(
        dry_run.status.success(),
        "{}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let preview = String::from_utf8_lossy(&dry_run.stdout);
    assert!(preview.contains(&format!("root-onto={}", stack.main_after)));
    assert!(preview.contains(&format!("old-base={}", stack.main_before)));
    for (worktree, head) in stack.worktrees.iter().zip(&stack.heads_before) {
        assert!(preview.contains(&format!("worktree={}", worktree.display())));
        assert!(preview.contains(&format!("before-head={head}")));
    }
    assert!(preview.contains("onto=deferred-after-step-1:feat/a"));
    assert!(preview.contains("command=git rebase --onto <after-step-1-head>"));
    assert!(!stack.rebase_log.exists());
    let state_after_preview: serde_json::Value =
        serde_json::from_slice(&fs::read(stack.state_dir.join("state.json")).unwrap()).unwrap();
    assert!(state_after_preview["rebase_progress"].is_null());

    let output = run_shirei(&stack, &args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("completed stack rebase for feat/c"));

    let heads_after = stack
        .worktrees
        .iter()
        .map(|worktree| git(worktree, &["rev-parse", "HEAD"]))
        .collect::<Vec<_>>();
    let log = fs::read_to_string(&stack.rebase_log).unwrap();
    let commands = log.lines().collect::<Vec<_>>();
    assert_eq!(commands.len(), 3, "{log}");
    assert!(commands[0].contains(&format!(
        "-C {} rebase --onto {} {}",
        stack.worktrees[0].display(),
        stack.main_after,
        stack.main_before
    )));
    assert!(commands[1].contains(&format!(
        "-C {} rebase --onto {} {}",
        stack.worktrees[1].display(),
        heads_after[0],
        stack.heads_before[0]
    )));
    assert!(commands[2].contains(&format!(
        "-C {} rebase --onto {} {}",
        stack.worktrees[2].display(),
        heads_after[1],
        stack.heads_before[1]
    )));
    assert!(commands
        .iter()
        .all(|command| !command.contains(&format!("-C {} rebase", stack.repo.display()))));

    assert_eq!(
        git(&stack.repo, &["rev-list", "--count", "main..feat/a"]),
        "1"
    );
    assert_eq!(
        git(&stack.repo, &["rev-list", "--count", "feat/a..feat/b"]),
        "1"
    );
    assert_eq!(
        git(&stack.repo, &["rev-list", "--count", "feat/b..feat/c"]),
        "1"
    );
    assert_eq!(
        git(&stack.repo, &["log", "-1", "--format=%s", "feat/b"]),
        "B"
    );
    assert_eq!(
        git(&stack.repo, &["log", "-1", "--format=%s", "feat/c"]),
        "C"
    );
    let old_a_is_ancestor = Command::new("git")
        .arg("-C")
        .arg(&stack.repo)
        .args([
            "merge-base",
            "--is-ancestor",
            &stack.heads_before[0],
            "feat/b",
        ])
        .status()
        .unwrap();
    assert!(!old_a_is_ancestor.success());

    let state_after: serde_json::Value =
        serde_json::from_slice(&fs::read(stack.state_dir.join("state.json")).unwrap()).unwrap();
    assert!(state_after["rebase_progress"].is_null());
}

#[test]
fn conflict_continue_resumes_the_same_worktree_rebase() {
    let stack = setup_stack(true, false);
    let repo_arg = stack.repo.to_string_lossy().to_string();
    let output = run_shirei(
        &stack,
        &["stack", "rebase", "--branch", "feat/c", "--repo", &repo_arg],
    );
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("status=conflict branch=feat/a"));
    let paused: serde_json::Value =
        serde_json::from_slice(&fs::read(stack.state_dir.join("state.json")).unwrap()).unwrap();
    assert_eq!(paused["rebase_progress"]["version"], 2);
    assert_eq!(paused["rebase_progress"]["steps"][0]["status"], "conflict");

    fs::write(stack.worktrees[0].join("README.md"), "resolved\n").unwrap();
    git(&stack.worktrees[0], &["add", "README.md"]);
    let continued = run_shirei(&stack, &["stack", "rebase", "--continue"]);
    assert!(
        continued.status.success(),
        "{}",
        String::from_utf8_lossy(&continued.stderr)
    );
    assert!(
        String::from_utf8_lossy(&continued.stdout).contains("completed stack rebase for feat/c")
    );
    assert_eq!(
        git(&stack.repo, &["rev-list", "--count", "main..feat/a"]),
        "1"
    );
    assert_eq!(
        git(&stack.repo, &["rev-list", "--count", "feat/a..feat/b"]),
        "1"
    );
    assert_eq!(
        git(&stack.repo, &["rev-list", "--count", "feat/b..feat/c"]),
        "1"
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(stack.state_dir.join("state.json")).unwrap()).unwrap();
    assert!(state["rebase_progress"].is_null());
}

#[test]
fn conflict_abort_only_aborts_the_recorded_worktree_rebase() {
    let stack = setup_stack(true, false);
    let repo_arg = stack.repo.to_string_lossy().to_string();
    let output = run_shirei(
        &stack,
        &["stack", "rebase", "--branch", "feat/c", "--repo", &repo_arg],
    );
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("status=conflict branch=feat/a"));

    let aborted = run_shirei(&stack, &["stack", "rebase", "--abort"]);
    assert!(
        aborted.status.success(),
        "{}",
        String::from_utf8_lossy(&aborted.stderr)
    );
    assert!(String::from_utf8_lossy(&aborted.stdout).contains("aborted stack rebase for feat/c"));
    assert_eq!(
        git(&stack.worktrees[0], &["rev-parse", "HEAD"]),
        stack.heads_before[0]
    );
    assert_eq!(
        git(&stack.worktrees[1], &["rev-parse", "HEAD"]),
        stack.heads_before[1]
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(stack.state_dir.join("state.json")).unwrap()).unwrap();
    assert!(state["rebase_progress"].is_null());
}

#[test]
fn abort_refuses_when_the_paused_branch_ref_moves() {
    let stack = setup_stack(true, false);
    let repo_arg = stack.repo.to_string_lossy().to_string();
    let output = run_shirei(
        &stack,
        &["stack", "rebase", "--branch", "feat/c", "--repo", &repo_arg],
    );
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("status=conflict branch=feat/a"));

    git(
        &stack.repo,
        &["update-ref", "refs/heads/feat/a", &stack.main_after],
    );
    let aborted = run_shirei(&stack, &["stack", "rebase", "--abort"]);
    assert!(!aborted.status.success());
    let stderr = String::from_utf8_lossy(&aborted.stderr);
    assert!(stderr.contains("stack branch feat/a moved"), "{stderr}");
    assert_eq!(
        git(&stack.repo, &["rev-parse", "refs/heads/feat/a"]),
        stack.main_after
    );
}

#[test]
fn continue_refuses_when_a_completed_parent_branch_moves() {
    let stack = setup_stack(true, true);
    let repo_arg = stack.repo.to_string_lossy().to_string();
    let output = run_shirei(
        &stack,
        &["stack", "rebase", "--branch", "feat/c", "--repo", &repo_arg],
    );
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("status=conflict branch=feat/a"));

    fs::write(stack.worktrees[0].join("README.md"), "resolved\n").unwrap();
    git(&stack.worktrees[0], &["add", "README.md"]);
    let continued = run_shirei(&stack, &["stack", "rebase", "--continue"]);
    assert!(continued.status.success());
    assert!(String::from_utf8_lossy(&continued.stdout).contains("status=conflict branch=feat/b"));
    let parent_after = git(&stack.repo, &["rev-parse", "feat/a"]);
    assert_ne!(parent_after, stack.main_after);

    git(
        &stack.repo,
        &["update-ref", "refs/heads/feat/a", &stack.main_after],
    );
    let rejected = run_shirei(&stack, &["stack", "rebase", "--continue"]);
    assert!(!rejected.status.success());
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(
        stderr.contains("completed parent branch feat/a moved"),
        "{stderr}"
    );
    assert_eq!(
        git(&stack.repo, &["rev-parse", "refs/heads/feat/b"]),
        stack.heads_before[1]
    );
}
