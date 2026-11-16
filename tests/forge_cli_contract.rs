#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct TestRepo {
    _temp: TempDir,
    repo: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    git_log: PathBuf,
    state_dir: PathBuf,
    config: PathBuf,
}

// Flags listed by `glab mr update --help` and `glab mr create --help` in glab 1.108.0.
const GLAB_MR_UPDATE_FLAGS: &[&str] = &[
    "-a",
    "--assignee",
    "-d",
    "--description",
    "--draft",
    "-f",
    "--fill",
    "--fill-commit-body",
    "-l",
    "--label",
    "--lock-discussion",
    "-m",
    "--milestone",
    "-r",
    "--ready",
    "--remove-source-branch",
    "-R",
    "--repo",
    "--reviewer",
    "--squash-before-merge",
    "--target-branch",
    "-t",
    "--title",
    "--unassign",
    "-u",
    "--unlabel",
    "--unlock-discussion",
    "--wip",
    "-y",
    "--yes",
];

const GLAB_MR_CREATE_FLAGS: &[&str] = &[
    "--allow-collaboration",
    "-a",
    "--assignee",
    "--auto-merge",
    "--copy-issue-labels",
    "--create-source-branch",
    "-d",
    "--description",
    "--draft",
    "-f",
    "--fill",
    "--fill-commit-body",
    "-H",
    "--head",
    "-l",
    "--label",
    "-m",
    "--milestone",
    "--no-editor",
    "--push",
    "--recover",
    "-i",
    "--related-issue",
    "--remove-source-branch",
    "-R",
    "--repo",
    "--reviewer",
    "--signoff",
    "-s",
    "--source-branch",
    "--squash-before-merge",
    "-b",
    "--target-branch",
    "--template",
    "-t",
    "--title",
    "-w",
    "--web",
    "--wip",
    "-y",
    "--yes",
];

/// A fake glab that rejects flags the real glab 1.108.0 does not accept, the
/// way glab reports `Unknown flag: --description-file`.
fn strict_fake_glab(view_json: &str) -> String {
    let flag_case = |flags: &[&str]| {
        flags
            .iter()
            .map(|flag| format!("{flag}|{flag}=*"))
            .collect::<Vec<_>>()
            .join("|")
    };
    format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$SHIREI_TEST_LOG"
check_flags() {{
  allowed="$1"
  shift
  for arg in "$@"; do
    case "$arg" in
      -*)
        if ! printf '%s' "$arg" | grep -Eq "^($allowed)$"; then
          printf 'Unknown flag: %s\n' "${{arg%%=*}}" >&2
          exit 1
        fi
        ;;
    esac
  done
}}
case "$1 $2" in
  "--version ") printf 'glab 1.108.0\n' ;;
  "mr list") printf '[]\n' ;;
  "mr create")
    shift 2
    check_flags '{create}' "$@"
    printf 'mr create env GLAB_NO_PROMPT=%s NO_PROMPT=%s\n' "$GLAB_NO_PROMPT" "$NO_PROMPT" >> "$SHIREI_TEST_LOG"
    printf 'https://gitlab.com/owner/repo/-/merge_requests/9\n'
    ;;
  "mr view") printf '%s\n' '{view_json}' ;;
  "mr update")
    shift 3
    check_flags '{update}' "$@"
    ;;
  *) exit 2 ;;
esac
"#,
        create = flag_case(GLAB_MR_CREATE_FLAGS).replace('*', ".*"),
        update = flag_case(GLAB_MR_UPDATE_FLAGS).replace('*', ".*"),
    )
}

fn git(repo: &Path, args: &[&str]) {
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
}

fn executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn test_repo(remote: &str) -> TestRepo {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let bin = temp.path().join("bin");
    let log = temp.path().join("forge.log");
    let git_log = temp.path().join("git.log");
    let state_dir = temp.path().join("state");
    let config = temp.path().join("config.toml");
    fs::create_dir(&repo).unwrap();
    fs::create_dir(&bin).unwrap();
    fs::create_dir(&state_dir).unwrap();
    fs::write(&config, "").unwrap();

    git(&repo, &["init", "--initial-branch=main"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    fs::write(repo.join("README.md"), "main\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "initial"]);
    git(&repo, &["remote", "add", "origin", remote]);
    git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(&repo, &["switch", "-c", "feature/review"]);
    fs::write(repo.join("README.md"), "main\nfeature\n").unwrap();
    git(&repo, &["commit", "-am", "add feature"]);

    let git_path = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    executable(
        &bin.join("git"),
        &format!(
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = push ]; then\n    printf '%s\\n' \"$*\" >> \"$SHIREI_GIT_LOG\"\n    exit 0\n  fi\n  if [ \"$arg\" = ls-remote ]; then\n    [ -n \"$SHIREI_LS_REMOTE_OUTPUT\" ] || exit 128\n    printf '%b' \"$SHIREI_LS_REMOTE_OUTPUT\"\n    exit 0\n  fi\ndone\nexec \"{}\" \"$@\"\n",
            git_path.trim()
        ),
    );

    TestRepo {
        _temp: temp,
        repo: fs::canonicalize(repo).unwrap(),
        bin,
        log,
        git_log,
        state_dir,
        config,
    }
}

fn track_review_branch_as_stack(test: &TestRepo) {
    let timestamp = "2026-01-01T00:00:00Z";
    let state = serde_json::json!({
        "recent": [],
        "worktrees": [
            {"path": test.repo, "repo": test.repo, "branch": "feature/review", "created": timestamp}
        ],
        "stack_branches": [
            {"repo": test.repo, "branch": "feature/review", "base": "main", "created": timestamp, "updated": timestamp}
        ]
    });
    fs::write(
        test.state_dir.join("state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();
}

fn run_shirei(test: &TestRepo, args: &[&str]) -> Output {
    shirei(test, args).output().unwrap()
}

fn shirei(test: &TestRepo, args: &[&str]) -> Command {
    let path = format!(
        "{}:{}",
        test.bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_shirei"));
    command
        .args(args)
        .current_dir(&test.repo)
        .env("PATH", path)
        .env("SHIREI_TEST_LOG", &test.log)
        .env("SHIREI_GIT_LOG", &test.git_log)
        .env("SHIREI_STATE_DIR", &test.state_dir)
        .env("SHIREI_CONFIG", &test.config)
        .env_remove("SHIREI_LS_REMOTE_OUTPUT")
        .env_remove("NO_PROMPT");
    command
}

/// feature/parent (review #5) <- feature/review <- feature/child (no review).
fn track_three_branch_stack(test: &TestRepo) {
    git(&test.repo, &["branch", "feature/parent", "main"]);
    git(&test.repo, &["branch", "feature/child", "feature/review"]);
    let timestamp = "2026-01-01T00:00:00Z";
    let state = serde_json::json!({
        "repos_keyed_by_main_worktree": true,
        "stack_branches": [
            {"repo": test.repo, "branch": "feature/parent", "base": "main", "review_id": "5", "review_url": "https://github.com/owner/repo/pull/5", "created": timestamp, "updated": timestamp},
            {"repo": test.repo, "branch": "feature/review", "base": "feature/parent", "created": timestamp, "updated": timestamp},
            {"repo": test.repo, "branch": "feature/child", "base": "feature/review", "created": timestamp, "updated": timestamp}
        ]
    });
    fs::write(
        test.state_dir.join("state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();
}

const STACK_FAKE_GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$SHIREI_TEST_LOG"
case "$1 $2" in
  "--version ") printf 'gh version 2.96.0\n' ;;
  "pr list")
    case "$*" in
      *"--head feature/parent "*) printf '[{"number":5,"url":"https://github.com/owner/repo/pull/5","baseRefName":"main","headRepository":{"name":"repo"},"headRepositoryOwner":{"login":"owner"}}]\n' ;;
      *) printf '[]\n' ;;
    esac
    ;;
  "pr create") printf 'https://github.com/owner/repo/pull/7\n' ;;
  "pr view")
    case "$*" in
      *"--json body"*) printf '{"body":"Parent body"}\n' ;;
      *) printf '{"number":7,"url":"https://github.com/owner/repo/pull/7"}\n' ;;
    esac
    ;;
  "pr edit")
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --body-file ]; then
        printf 'edit body:\n' >> "$SHIREI_TEST_LOG"
        cat "$2" >> "$SHIREI_TEST_LOG"
        printf '\n' >> "$SHIREI_TEST_LOG"
      fi
      shift
    done
    ;;
  *) exit 2 ;;
esac
"#;

#[test]
fn pr_create_on_a_stack_branch_links_every_open_review_both_ways() {
    let test = test_repo("git@github.com:owner/repo.git");
    track_three_branch_stack(&test);
    executable(&test.bin.join("gh"), STACK_FAKE_GH);

    let preview = run_shirei(
        &test,
        &[
            "pr",
            "create",
            "--dry-run",
            "--title",
            "Add feature",
            "--body",
            "Review body",
        ],
    );
    let stdout = String::from_utf8_lossy(&preview.stdout);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    assert!(stdout.contains("\nbase=feature/parent\n"), "{stdout}");
    assert!(stdout.contains("stack=2 of 3\n"), "{stdout}");
    assert!(stdout.contains("stack_links_refresh=feature/parent,feature/child\n"));
    assert!(stdout.contains(r#"Review body\n\n<!-- shirei-stack-links:start -->\nStack: 2 of 3\n- Previous: [feature/parent](https://github.com/owner/repo/pull/5)\n- Next: feature/child (no review yet)"#), "{stdout}");
    assert!(!test.log.exists());

    let output = run_shirei(
        &test,
        &[
            "pr",
            "create",
            "--title",
            "Add feature",
            "--body",
            "Review body",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = fs::read_to_string(&test.log).unwrap();
    assert!(
        log.contains("pr create --base feature/parent --head feature/review"),
        "{log}"
    );
    assert!(log.contains("--body Review body\n\n<!-- shirei-stack-links:start -->\nStack: 2 of 3\n- Previous: [feature/parent](https://github.com/owner/repo/pull/5)"), "{log}");
    assert!(log.contains("pr edit 5 --body-file"), "{log}");
    assert!(
        log.contains("edit body:\nParent body\n\n<!-- shirei-stack-links:start -->\nStack: 1 of 3\n- Previous: none\n- Next: [feature/review](https://github.com/owner/repo/pull/7)\n\nMerge order:\n1. **feature/parent** (this review)\n2. [feature/review](https://github.com/owner/repo/pull/7)\n3. feature/child (no review yet)\n<!-- shirei-stack-links:end -->"),
        "{log}"
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(test.state_dir.join("state.json")).unwrap()).unwrap();
    let review = state["stack_branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["branch"] == "feature/review")
        .unwrap();
    assert_eq!(review["review_url"], "https://github.com/owner/repo/pull/7");
}

fn dry_run_title(test: &TestRepo, title: &str) -> (String, String) {
    let output = run_shirei(
        test,
        &[
            "pr",
            "create",
            "--dry-run",
            "--title",
            title,
            "--body",
            "Why.",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let title = stdout
        .lines()
        .find_map(|line| line.strip_prefix("title="))
        .unwrap()
        .to_string();
    (title, String::from_utf8_lossy(&output.stderr).to_string())
}

#[test]
fn pr_create_adds_the_ticket_prefix_recent_titles_use() {
    let test = test_repo("git@gitlab.com:owner/repo.git");
    git(&test.repo, &["switch", "main"]);
    for (index, subject) in ["ABC-11: one", "ABC-12: two", "ABC-13: three"]
        .iter()
        .enumerate()
    {
        fs::write(test.repo.join(format!("trunk-{index}.txt")), "x\n").unwrap();
        git(&test.repo, &["add", "-A"]);
        git(&test.repo, &["commit", "-m", subject]);
    }
    git(
        &test.repo,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );

    git(&test.repo, &["switch", "-c", "avenceslau/abc-1725-retries"]);
    fs::write(test.repo.join("retries.txt"), "x\n").unwrap();
    git(&test.repo, &["add", "-A"]);
    git(&test.repo, &["commit", "-m", "add retries"]);
    assert_eq!(
        dry_run_title(&test, "Add retries").0,
        r#""ABC-1725: Add retries""#
    );
    assert_eq!(
        dry_run_title(&test, "ABC-000: Add retries").0,
        r#""ABC-1725: Add retries""#
    );
    assert_eq!(
        dry_run_title(&test, "ABC-9: Add retries").0,
        r#""ABC-9: Add retries""#
    );

    git(&test.repo, &["switch", "-c", "retries-from-trailer"]);
    git(
        &test.repo,
        &["commit", "--amend", "-m", "add retries\n\nRefs: ABC-1800"],
    );
    assert_eq!(
        dry_run_title(&test, "Add retries").0,
        r#""ABC-1800: Add retries""#
    );

    git(&test.repo, &["commit", "--amend", "-m", "add retries"]);
    let (title, stderr) = dry_run_title(&test, "Add retries");
    assert_eq!(title, r#""Add retries""#);
    assert!(stderr.contains("no ABC ticket was found"), "{stderr}");

    git(&test.repo, &["config", "shirei.ticketPrefix", "none"]);
    let (_, stderr) = dry_run_title(&test, "Add retries");
    assert!(!stderr.contains("ticket"), "{stderr}");
}

#[test]
fn pr_create_rejects_internal_names_only_on_public_hosts() {
    let test = test_repo("git@github.com:owner/repo.git");
    let args = [
        "pr",
        "create",
        "--dry-run",
        "--title",
        "Fix Acme hook",
        "--body",
        "Found by acme-loadtests.\nNothing else.\nRuns on Rocket",
    ];

    let output = run_shirei(&test, &args);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("github.com is public"), "{stderr}");
    assert!(stderr.contains("title: acme"), "{stderr}");
    assert!(stderr.contains("body line 1: acme-loadtests"), "{stderr}");
    assert!(stderr.contains("body line 3: rocket"), "{stderr}");

    fs::write(
        &test.config,
        "[review]\nextra_public_denylist = [\"hook\"]\n",
    )
    .unwrap();
    let extended = run_shirei(&test, &args);
    assert!(String::from_utf8_lossy(&extended.stderr).contains("title: hook"));

    git(
        &test.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "git@gitlab.corp.example:owner/repo.git",
        ],
    );
    let private = run_shirei(&test, &args);
    assert!(
        private.status.success(),
        "{}",
        String::from_utf8_lossy(&private.stderr)
    );
}

#[test]
fn pr_create_enforces_the_word_budget_for_the_change_size() {
    let test = test_repo("git@github.com:owner/repo.git");
    let words = |count: usize| vec!["word"; count].join(" ");
    let run = |body: &str| {
        run_shirei(
            &test,
            &[
                "pr",
                "create",
                "--dry-run",
                "--title",
                "Add feature",
                "--body",
                body,
            ],
        )
    };

    let within = run(&format!("{}\n\n```\n{}\n```", words(80), words(50)));
    assert!(
        within.status.success(),
        "{}",
        String::from_utf8_lossy(&within.stderr)
    );
    assert!(String::from_utf8_lossy(&within.stdout).contains("body_words=80/80 (small)"));

    let over = run(&words(81));
    assert!(!over.status.success());
    let stderr = String::from_utf8_lossy(&over.stderr);
    assert!(
        stderr.contains("81 prose words; the budget for a small change (1 changed lines in 0 top-level directories against origin/main) is 80"),
        "{stderr}"
    );

    fs::write(&test.config, "[review]\nsmall_max_words = 100\n").unwrap();
    assert!(run(&words(81)).status.success());
}

#[test]
fn pr_create_rejects_a_hand_written_stack_section() {
    let test = test_repo("git@github.com:owner/repo.git");
    track_three_branch_stack(&test);

    let output = run_shirei(
        &test,
        &[
            "pr",
            "create",
            "--dry-run",
            "--title",
            "Add feature",
            "--body",
            "Why\n\n## Stack\n- after #5",
        ],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("`## Stack` heading"), "{stderr}");
    assert!(
        stderr.contains("Shirei adds the stack links block itself"),
        "{stderr}"
    );
}

#[test]
fn pr_create_selects_the_remote_from_flag_config_or_upstream() {
    let test = test_repo("git@github.com:owner/repo.git");
    git(
        &test.repo,
        &["remote", "add", "gitlab", "git@gitlab.com:owner/repo.git"],
    );
    git(
        &test.repo,
        &["update-ref", "refs/remotes/gitlab/main", "main"],
    );
    let remote_of = |args: &[&str]| {
        let output = run_shirei(&test, args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.strip_prefix("remote_target="))
            .unwrap()
            .to_string()
    };
    let dry_run = ["pr", "create", "--dry-run", "--base", "main"];

    assert_eq!(remote_of(&dry_run), "github.com/owner/repo");
    assert_eq!(
        remote_of(&[&dry_run[..], &["--remote", "gitlab"]].concat()),
        "gitlab.com/owner/repo"
    );
    git(&test.repo, &["config", "shirei.remote", "gitlab"]);
    assert_eq!(remote_of(&dry_run), "gitlab.com/owner/repo");
    git(
        &test.repo,
        &["config", "branch.feature/review.remote", "origin"],
    );
    assert_eq!(remote_of(&dry_run), "github.com/owner/repo");
}

#[test]
fn pr_create_prefers_the_remote_default_branch_over_a_stale_local_head() {
    let test = test_repo("git@github.com:owner/repo.git");
    git(
        &test.repo,
        &["update-ref", "refs/remotes/origin/staging", "main"],
    );
    git(
        &test.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/staging",
        ],
    );

    let output = shirei(&test, &["pr", "create", "--dry-run"])
        .env(
            "SHIREI_LS_REMOTE_OUTPUT",
            "ref: refs/heads/main\\tHEAD\\n0123456789012345678901234567890123456789\\tHEAD\\n",
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\nbase=main\n"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("refs/remotes/origin/HEAD points to staging, but origin now uses main"),
        "{stderr}"
    );

    let offline = run_shirei(&test, &["pr", "create", "--dry-run"]);
    assert!(String::from_utf8_lossy(&offline.stdout).contains("\nbase=staging\n"));
}

#[test]
fn gitlab_stack_submit_updates_descriptions_with_supported_flags() {
    let test = test_repo("git@gitlab.com:owner/repo.git");
    track_review_branch_as_stack(&test);
    executable(
        &test.bin.join("glab"),
        &strict_fake_glab(
            r#"{"iid":9,"web_url":"https://gitlab.com/owner/repo/-/merge_requests/9","description":"- starts like a flag"}"#,
        ),
    );

    let output = run_shirei(&test, &["stack", "submit", "--branch", "feature/review"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("submitted feature/review"));

    let log = fs::read_to_string(&test.log).unwrap();
    assert!(
        log.contains(
            "mr update 9 --description=- starts like a flag\n\n<!-- shirei-stack-links:start -->"
        ),
        "{log}"
    );
    assert!(log.contains("--repo ssh://git@gitlab.com/owner/repo"));
    assert!(!log.contains("--description-file"));
}

#[test]
fn stack_submit_force_pushes_with_a_lease_on_the_remote_commit_seen_first() {
    let test = test_repo("git@gitlab.com:owner/repo.git");
    track_review_branch_as_stack(&test);
    git(
        &test.repo,
        &["update-ref", "refs/remotes/origin/feature/review", "main"],
    );
    let previously_pushed = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&test.repo)
            .args(["rev-parse", "main"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    executable(
        &test.bin.join("glab"),
        &strict_fake_glab(
            r#"{"iid":9,"web_url":"https://gitlab.com/owner/repo/-/merge_requests/9","description":""}"#,
        ),
    );

    let output = run_shirei(&test, &["stack", "submit", "--branch", "feature/review"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pushes = fs::read_to_string(&test.git_log).unwrap();
    assert!(
        pushes.contains(&format!(
            "push -u --force-with-lease=feature/review:{} origin feature/review",
            previously_pushed.trim()
        )),
        "{pushes}"
    );
}

#[test]
fn installed_glab_accepts_the_flags_shirei_uses() {
    let Ok(update_help) = Command::new("glab")
        .args(["mr", "update", "--help"])
        .output()
    else {
        eprintln!("glab is not installed; skipping");
        return;
    };
    let create_help = Command::new("glab")
        .args(["mr", "create", "--help"])
        .output()
        .unwrap();
    let update_help = String::from_utf8_lossy(&update_help.stdout).to_string()
        + &String::from_utf8_lossy(&update_help.stderr);
    let create_help = String::from_utf8_lossy(&create_help.stdout).to_string()
        + &String::from_utf8_lossy(&create_help.stderr);

    for flag in ["--description", "--repo"] {
        assert!(update_help.contains(flag), "glab mr update lacks {flag}");
    }
    for flag in [
        "--source-branch",
        "--target-branch",
        "--title",
        "--description",
        "--yes",
        "--repo",
        "--head",
        "--draft",
    ] {
        assert!(create_help.contains(flag), "glab mr create lacks {flag}");
    }
    assert!(!update_help.contains("--description-file"));
}

#[test]
fn github_contract_uses_explicit_json_and_repository() {
    let test = test_repo("git@github.com:owner/repo.git");
    executable(
        &test.bin.join("gh"),
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$SHIREI_TEST_LOG"
case "$1 $2" in
  "--version ") printf 'gh version 2.96.0\n' ;;
  "pr list") printf '[]\n' ;;
  "pr create") printf 'https://github.com/owner/repo/pull/7\n' ;;
  "pr view") printf '{"number":7,"url":"https://github.com/owner/repo/pull/7"}\n' ;;
  *) exit 2 ;;
esac
"#,
    );

    let output = run_shirei(
        &test,
        &[
            "pr",
            "create",
            "--title",
            "Add feature",
            "--body",
            "Review body",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("url=https://github.com/owner/repo/pull/7"));

    let log = fs::read_to_string(&test.log).unwrap();
    assert!(log.contains("--version"));
    assert!(log.contains("pr list --head feature/review --base main --state open --limit 100 --json number,url,baseRefName,headRepository,headRepositoryOwner --repo github.com/owner/repo"));
    assert!(log.contains("pr create --base main --head feature/review --fill --repo github.com/owner/repo --title Add feature --body Review body"));
    assert!(log.contains("pr view https://github.com/owner/repo/pull/7 --json number,url --repo github.com/owner/repo"));
}

#[test]
fn gitlab_contract_uses_json_without_implicit_push() {
    let test = test_repo("git@gitlab.com:owner/repo.git");
    executable(
        &test.bin.join("glab"),
        &strict_fake_glab(
            r#"{"iid":9,"web_url":"https://gitlab.com/owner/repo/-/merge_requests/9"}"#,
        ),
    );

    let output = run_shirei(
        &test,
        &[
            "pr",
            "create",
            "--title",
            "Add feature",
            "--body",
            "Review body",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout)
        .contains("url=https://gitlab.com/owner/repo/-/merge_requests/9"));

    let log = fs::read_to_string(&test.log).unwrap();
    assert!(log.contains("--version"));
    assert!(log.contains("mr list --source-branch feature/review --target-branch main --per-page 100 --output json --repo ssh://git@gitlab.com/owner/repo"));
    assert!(log.contains("mr create --source-branch feature/review --target-branch main --title Add feature --description Review body --yes --repo ssh://git@gitlab.com/owner/repo --head ssh://git@gitlab.com/owner/repo"));
    assert!(
        log.contains("mr create env GLAB_NO_PROMPT=1 NO_PROMPT=\n"),
        "{log}"
    );
    assert!(!log.contains("--fill"));
    assert!(!log.contains("--push"));
    assert!(log.contains("mr view 9 --output json --repo ssh://git@gitlab.com/owner/repo"));
}

#[test]
fn unsupported_cli_version_fails_before_review_calls() {
    let test = test_repo("git@github.com:owner/repo.git");
    executable(
        &test.bin.join("gh"),
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$SHIREI_TEST_LOG"
printf 'gh version 2.39.9\n'
"#,
    );

    let output = run_shirei(&test, &["pr", "create"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Shirei requires gh >= 2.40.0"), "{stderr}");

    let log = fs::read_to_string(&test.log).unwrap();
    assert_eq!(log.trim(), "--version");
    assert!(!test.git_log.exists());
}

#[test]
fn unsupported_glab_version_fails_before_push() {
    let test = test_repo("git@gitlab.com:owner/repo.git");
    executable(
        &test.bin.join("glab"),
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$SHIREI_TEST_LOG"
printf 'glab 1.49.9\n'
"#,
    );

    let output = run_shirei(&test, &["pr", "create"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Shirei requires glab >= 1.50.0"),
        "{stderr}"
    );
    assert!(!test.git_log.exists());
}

#[test]
fn unrelated_version_tokens_are_rejected() {
    let test = test_repo("git@github.com:owner/repo.git");
    executable(
        &test.bin.join("gh"),
        r#"#!/bin/sh
printf 'built 2026.07.02\n'
"#,
    );

    let output = run_shirei(&test, &["pr", "create"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("could not parse gh version"), "{stderr}");
    assert!(!test.git_log.exists());
}

#[test]
fn minimum_prerelease_version_is_rejected() {
    let test = test_repo("git@github.com:owner/repo.git");
    executable(
        &test.bin.join("gh"),
        r#"#!/bin/sh
printf 'gh version 2.40.0-rc1\n'
"#,
    );

    let output = run_shirei(&test, &["pr", "create"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Shirei requires gh >= 2.40.0"), "{stderr}");
    assert!(!test.git_log.exists());
}
