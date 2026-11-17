#![cfg(unix)]

use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct Repo {
    _temp: TempDir,
    root: PathBuf,
    main: PathBuf,
    worktree_root: PathBuf,
    state_dir: PathBuf,
    config: PathBuf,
    bin: PathBuf,
    backend_log: PathBuf,
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

fn executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn setup() -> Repo {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let main = root.join("repo");
    let worktree_root = root.join("worktrees");
    let state_dir = root.join("state");
    let config = root.join("config.toml");
    let bin = root.join("bin");
    let backend_log = root.join("backend.log");
    fs::create_dir(&main).unwrap();
    fs::create_dir(&state_dir).unwrap();
    fs::create_dir(&bin).unwrap();
    fs::write(
        &config,
        format!(
            "backend = \"tmux\"\nroots = []\nworktree_root = \"{}\"\nrun_install_on_create = false\n",
            worktree_root.display()
        ),
    )
    .unwrap();
    for backend in ["tmux", "herdr"] {
        executable(
            &bin.join(backend),
            &format!(
                "#!/bin/sh\nprintf '{backend} %s\\n' \"$*\" >> \"$SHIREI_BACKEND_LOG\"\nexit 0\n"
            ),
        );
    }

    git(&main, &["init", "--initial-branch=main"]);
    git(&main, &["config", "user.email", "test@example.com"]);
    git(&main, &["config", "user.name", "Test User"]);
    fs::write(main.join("README.md"), "main\n").unwrap();
    git(&main, &["add", "README.md"]);
    git(&main, &["commit", "-m", "initial"]);

    Repo {
        _temp: temp,
        root,
        main,
        worktree_root,
        state_dir,
        config,
        bin,
        backend_log,
    }
}

fn shirei(repo: &Repo, cwd: &Path, args: &[&str]) -> Command {
    let path = format!(
        "{}:{}",
        repo.bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_shirei"));
    command
        .args(args)
        .current_dir(cwd)
        .env("PATH", path)
        .env("SHIREI_STATE_DIR", &repo.state_dir)
        .env("SHIREI_CONFIG", &repo.config)
        .env("SHIREI_BACKEND_LOG", &repo.backend_log)
        .env_remove("PI_CODING_AGENT")
        .env_remove("TMUX");
    command
}

fn run_shirei(repo: &Repo, cwd: &Path, args: &[&str]) -> Output {
    shirei(repo, cwd, args).output().unwrap()
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

fn backend_calls(repo: &Repo) -> String {
    fs::read_to_string(&repo.backend_log).unwrap_or_default()
}

#[test]
fn no_open_print_path_creates_and_registers_without_touching_the_backend() {
    let repo = setup();

    let stdout = assert_success(&run_shirei(
        &repo,
        &repo.main,
        &[
            "branch",
            "feat/agent",
            "--base",
            "main",
            "--no-open",
            "--print-path",
        ],
    ));

    let expected = repo.worktree_root.join("repo").join("feat-agent");
    assert_eq!(stdout, format!("{}\n", expected.display()));
    assert_eq!(git(&expected, &["branch", "--show-current"]), "feat/agent");
    assert_eq!(backend_calls(&repo), "");
    let state = read_state(&repo);
    assert_eq!(state["worktrees"][0]["path"], json!(expected));
    assert_eq!(state["worktrees"][0]["repo"], json!(repo.main));
}

#[test]
fn non_terminal_stdin_defaults_to_no_open_and_open_overrides_it() {
    let repo = setup();

    let stdout = assert_success(&run_shirei(
        &repo,
        &repo.main,
        &["branch", "feat/quiet", "--base", "main"],
    ));
    assert!(stdout.starts_with("created\tpath="), "{stdout}");
    assert_eq!(backend_calls(&repo), "");

    let mut agent = shirei(
        &repo,
        &repo.main,
        &["branch", "feat/agent-env", "--base", "main"],
    );
    assert_success(&agent.env("PI_CODING_AGENT", "true").output().unwrap());
    assert_eq!(backend_calls(&repo), "");

    assert_success(&run_shirei(
        &repo,
        &repo.main,
        &["branch", "feat/opened", "--base", "main", "--open"],
    ));
    let calls = backend_calls(&repo);
    assert!(calls.contains("tmux new-session"), "{calls}");
    assert!(calls.contains("tmux attach-session"), "{calls}");
}

#[test]
fn existing_branch_checked_out_elsewhere_is_reused_at_its_real_path() {
    let repo = setup();
    let elsewhere = repo.root.join("elsewhere");
    let elsewhere_arg = elsewhere.to_string_lossy().to_string();
    git(
        &repo.main,
        &["worktree", "add", "-b", "feat/elsewhere", &elsewhere_arg],
    );

    let stdout = assert_success(&run_shirei(
        &repo,
        &repo.main,
        &["branch", "feat/elsewhere", "--no-open", "--print-path"],
    ));

    assert_eq!(stdout, format!("{}\n", elsewhere.display()));
    assert!(!repo
        .worktree_root
        .join("repo")
        .join("feat-elsewhere")
        .exists());
    let state = read_state(&repo);
    assert_eq!(state["worktrees"].as_array().unwrap().len(), 1);
    assert_eq!(state["worktrees"][0]["path"], json!(elsewhere));
}

#[test]
fn stack_branch_from_a_linked_worktree_is_recorded_under_the_main_checkout() {
    let repo = setup();
    let parent = PathBuf::from(
        assert_success(&run_shirei(
            &repo,
            &repo.main,
            &[
                "stack",
                "branch",
                "feat/parent",
                "--base",
                "main",
                "--print-path",
            ],
        ))
        .trim(),
    );

    let child = assert_success(&run_shirei(
        &repo,
        &parent,
        &[
            "stack",
            "branch",
            "feat/child",
            "--base",
            "feat/parent",
            "--no-open",
            "--print-path",
        ],
    ));
    assert!(
        child.trim().ends_with("worktrees/repo/feat-child"),
        "{child}"
    );

    let listed = assert_success(&run_shirei(&repo, &repo.main, &["stack", "list"]));
    assert!(
        listed.contains(&format!("repo={}", repo.main.display())),
        "{listed}"
    );
    assert!(listed.contains("feat/child\tbase=feat/parent"), "{listed}");
    assert!(!listed.contains("missing-worktree"), "{listed}");
}

#[test]
fn sparse_and_checkout_exclude_limit_the_new_worktree() {
    let repo = setup();
    fs::create_dir_all(repo.main.join("docs")).unwrap();
    fs::create_dir_all(repo.main.join("src")).unwrap();
    fs::write(repo.main.join("docs/guide.md"), "guide\n").unwrap();
    fs::write(repo.main.join("docs/.dev.vars.example"), "KEY=\n").unwrap();
    fs::write(repo.main.join("src/lib.rs"), "\n").unwrap();
    git(&repo.main, &["add", "-A"]);
    git(&repo.main, &["commit", "-m", "tree"]);
    let mut config = fs::read_to_string(&repo.config).unwrap();
    config.push_str("checkout_exclude = [\".dev.vars*\"]\n");
    fs::write(&repo.config, config).unwrap();

    let stdout = assert_success(&run_shirei(
        &repo,
        &repo.main,
        &[
            "branch",
            "feat/docs",
            "--base",
            "main",
            "--no-open",
            "--print-path",
            "--sparse",
            "docs",
        ],
    ));

    let worktree = PathBuf::from(stdout.trim());
    assert!(worktree.join("README.md").exists());
    assert!(worktree.join("docs/guide.md").exists());
    assert!(!worktree.join("docs/.dev.vars.example").exists());
    assert!(!worktree.join("src").exists());
    assert_eq!(git(&worktree, &["status", "--porcelain"]), "");
}

#[test]
fn install_sync_runs_setup_in_the_foreground_and_reports_its_status() {
    let repo = setup();
    fs::write(
        repo.main.join("Makefile"),
        "install:\n\t@echo setup-output\n\t@test ! -e fail-install\n\t@touch installed\n",
    )
    .unwrap();
    git(&repo.main, &["add", "Makefile"]);
    git(&repo.main, &["commit", "-m", "makefile"]);

    let output = run_shirei(
        &repo,
        &repo.main,
        &[
            "branch",
            "feat/setup",
            "--base",
            "main",
            "--no-open",
            "--print-path",
            "--install=sync",
        ],
    );
    let stdout = assert_success(&output);
    let worktree = PathBuf::from(stdout.trim());
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    assert!(worktree.join("installed").exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("setup-output"), "{stderr}");
    assert!(
        stderr.contains("`make install` exited with status 0"),
        "{stderr}"
    );

    fs::write(repo.main.join("fail-install"), "").unwrap();
    git(&repo.main, &["add", "fail-install"]);
    git(&repo.main, &["commit", "-m", "fail install"]);
    let failed = run_shirei(
        &repo,
        &repo.main,
        &[
            "branch",
            "feat/broken-setup",
            "--base",
            "main",
            "--no-open",
            "--print-path",
            "--install=sync",
        ],
    );
    assert!(!failed.status.success());
    let worktree = PathBuf::from(String::from_utf8_lossy(&failed.stdout).trim());
    assert!(worktree.is_dir());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("failed with status"));
    let state = read_state(&repo);
    assert_eq!(state["worktrees"].as_array().unwrap().len(), 2);
    assert_eq!(backend_calls(&repo), "");
}

fn write_state(repo: &Repo, state: Value) {
    fs::write(
        repo.state_dir.join("state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();
}

#[test]
fn scan_keeps_entries_of_repos_that_fail_to_scan() {
    let repo = setup();
    let live = repo.root.join("live");
    let live_arg = live.to_string_lossy().to_string();
    git(
        &repo.main,
        &["worktree", "add", "-b", "feat/live", &live_arg],
    );
    let deleted_repo = repo.root.join("deleted-repo");
    let orphan = repo.root.join("orphan");
    let timestamp = "2026-01-01T00:00:00Z";
    write_state(
        &repo,
        json!({
            "repos_keyed_by_main_worktree": true,
            "worktrees": [
                {"path": live, "repo": repo.main, "branch": "feat/live", "created": timestamp},
                {"path": orphan, "repo": deleted_repo, "branch": "feat/orphan", "created": timestamp}
            ]
        }),
    );

    let output = run_shirei(&repo, &repo.main, &["worktrees", "scan"]);
    let stdout = assert_success(&output);
    assert!(
        stdout.contains("kept 1 from repos that failed to scan"),
        "{stdout}"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("deleted-repo"));
    let paths: Vec<Value> = read_state(&repo)["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .map(|worktree| worktree["path"].clone())
        .collect();
    assert!(paths.contains(&json!(live)));
    assert!(paths.contains(&json!(orphan)));

    let pruned = assert_success(&run_shirei(
        &repo,
        &repo.main,
        &["worktrees", "prune", "--missing"],
    ));
    assert!(
        pruned.contains(&format!("removed {}", orphan.display())),
        "{pruned}"
    );
    let state = read_state(&repo);
    assert_eq!(state["worktrees"].as_array().unwrap().len(), 1);
    assert_eq!(state["worktrees"][0]["path"], json!(live));
    assert!(live.is_dir());
}

#[test]
fn prune_merged_previews_then_deletes_only_clean_merged_worktrees() {
    let repo = setup();
    let main_arg = repo.main.to_string_lossy().to_string();
    git(&repo.main, &["remote", "add", "origin", &main_arg]);
    let mut paths = Vec::new();
    for name in ["merged", "dirty", "unmerged"] {
        let stdout = assert_success(&run_shirei(
            &repo,
            &repo.main,
            &[
                "branch",
                &format!("feat/{name}"),
                "--base",
                "main",
                "--print-path",
            ],
        ));
        let path = PathBuf::from(stdout.trim());
        fs::write(path.join(format!("{name}.txt")), "work\n").unwrap();
        git(&path, &["add", "-A"]);
        git(&path, &["commit", "-m", name]);
        paths.push(path);
    }
    // feat/merged and feat/dirty landed on the remote default branch.
    git(
        &repo.main,
        &["merge", "--no-edit", "feat/merged", "feat/dirty"],
    );
    git(
        &repo.main,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    git(
        &repo.main,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    fs::write(paths[1].join("uncommitted.txt"), "wip\n").unwrap();

    let preview = assert_success(&run_shirei(
        &repo,
        &repo.main,
        &["worktrees", "prune", "--merged"],
    ));
    assert!(
        preview.contains(&format!(
            "would remove {}\tbranch=feat/merged",
            paths[0].display()
        )),
        "{preview}"
    );
    assert!(preview.contains(&format!("skip {}\treason=dirty", paths[1].display())));
    assert!(preview.contains(&format!("skip {}\treason=unmerged", paths[2].display())));
    assert!(paths.iter().all(|path| path.is_dir()));

    let only_unmerged = paths[2].to_string_lossy().to_string();
    assert_success(&run_shirei(
        &repo,
        &repo.main,
        &["worktrees", "prune", "--merged", "--yes", &only_unmerged],
    ));
    assert!(paths[0].is_dir());

    let removed = assert_success(&run_shirei(
        &repo,
        &repo.main,
        &["worktrees", "prune", "--merged", "--yes"],
    ));
    assert!(
        removed.contains(&format!("removed {}", paths[0].display())),
        "{removed}"
    );
    assert!(!paths[0].exists());
    assert!(paths[1].is_dir() && paths[2].is_dir());
    let branches = git(&repo.main, &["branch", "--format=%(refname:short)"]);
    assert!(!branches.contains("feat/merged"), "{branches}");
    assert!(branches.contains("feat/dirty") && branches.contains("feat/unmerged"));
    let state = read_state(&repo);
    assert_eq!(state["worktrees"].as_array().unwrap().len(), 2);
}

#[test]
fn untrack_accepts_the_path_of_a_deleted_repo() {
    let repo = setup();
    let deleted_repo = repo.root.join("deleted-repo");
    let timestamp = "2026-01-01T00:00:00Z";
    write_state(
        &repo,
        json!({
            "repos_keyed_by_main_worktree": true,
            "stack_branches": [
                {"repo": deleted_repo, "branch": "feat/gone", "base": "main", "created": timestamp, "updated": timestamp}
            ]
        }),
    );

    let deleted_arg = deleted_repo.to_string_lossy().to_string();
    let stdout = assert_success(&run_shirei(
        &repo,
        &repo.root,
        &["stack", "untrack", "feat/gone", "--repo", &deleted_arg],
    ));
    assert!(stdout.starts_with("untracked feat/gone"), "{stdout}");
    assert_eq!(
        read_state(&repo)["stack_branches"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn worktrees_add_registers_one_worktree_created_with_git() {
    let repo = setup();
    let manual = repo.root.join("manual");
    let manual_arg = manual.to_string_lossy().to_string();
    git(
        &repo.main,
        &["worktree", "add", "-b", "feat/manual", &manual_arg],
    );
    assert_success(&run_shirei(
        &repo,
        &repo.main,
        &["stack", "track", "feat/manual", "--base", "main"],
    ));

    let added = assert_success(&run_shirei(
        &repo,
        &manual,
        &["worktrees", "add", &manual_arg],
    ));
    assert!(
        added.starts_with(&format!("added {}", manual.display())),
        "{added}"
    );

    let listed = assert_success(&run_shirei(&repo, &repo.main, &["stack", "list"]));
    assert!(
        listed.contains(&format!("path={}", manual.display())),
        "{listed}"
    );
    assert!(!listed.contains("missing-worktree"), "{listed}");

    let main_arg = repo.main.to_string_lossy().to_string();
    let refused = run_shirei(&repo, &repo.main, &["worktrees", "add", &main_arg]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("main checkout"));
}
