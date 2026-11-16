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
