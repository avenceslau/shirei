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
    fs::create_dir(&repo).unwrap();
    fs::create_dir(&bin).unwrap();

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
            "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = push ]; then\n    printf '%s\\n' \"$*\" >> \"$SHIREI_GIT_LOG\"\n    exit 0\n  fi\ndone\nexec \"{}\" \"$@\"\n",
            git_path.trim()
        ),
    );

    TestRepo {
        _temp: temp,
        repo,
        bin,
        log,
        git_log,
    }
}

fn run_shirei(test: &TestRepo, args: &[&str]) -> Output {
    let path = format!(
        "{}:{}",
        test.bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(env!("CARGO_BIN_EXE_shirei"))
        .args(args)
        .current_dir(&test.repo)
        .env("PATH", path)
        .env("SHIREI_TEST_LOG", &test.log)
        .env("SHIREI_GIT_LOG", &test.git_log)
        .output()
        .unwrap()
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
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$SHIREI_TEST_LOG"
case "$1 $2" in
  "--version ") printf 'glab 1.108.0\n' ;;
  "mr list") printf '[]\n' ;;
  "mr create") printf 'https://gitlab.com/owner/repo/-/merge_requests/9\n' ;;
  "mr view") printf '{"iid":9,"web_url":"https://gitlab.com/owner/repo/-/merge_requests/9"}\n' ;;
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
        .contains("url=https://gitlab.com/owner/repo/-/merge_requests/9"));

    let log = fs::read_to_string(&test.log).unwrap();
    assert!(log.contains("--version"));
    assert!(log.contains("mr list --source-branch feature/review --target-branch main --per-page 100 --output json --repo ssh://git@gitlab.com/owner/repo"));
    assert!(log.contains("mr create --source-branch feature/review --target-branch main --title Add feature --description Review body --yes --repo ssh://git@gitlab.com/owner/repo"));
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
