use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct Branch {
    /// Display name (e.g. `feat/foo` or `origin/feat/foo` for remote-only).
    pub display: String,
    /// Local branch name to use when creating/attaching a worktree.
    pub local_name: String,
    pub local: bool,
    pub remote: Option<String>,
}

pub fn repo_root(start: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("failed to run git rev-parse")?;
    if !output.status.success() {
        return Err(anyhow!(
            "not inside a git repo (cwd={}): {}",
            start.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(root))
}

pub fn common_dir(start: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(start)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .context("failed to run git rev-parse --git-common-dir")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git rev-parse --git-common-dir failed in {}: {}",
            start.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let path = if value.is_absolute() {
        value
    } else {
        start.join(value)
    };
    std::fs::canonicalize(&path)
        .with_context(|| format!("canonicalize git common dir {}", path.display()))
}

/// The repository's main worktree. Shirei keys state by it so that every
/// linked worktree of one repository resolves to the same repository.
pub fn main_worktree(start: &Path) -> Result<PathBuf> {
    let common_dir = common_dir(start)?;
    if common_dir.file_name().is_some_and(|name| name == ".git") {
        if let Some(parent) = common_dir.parent() {
            return Ok(parent.to_path_buf());
        }
    }

    // Bare repositories and separate git dirs have no `.git` directory to
    // take the parent of; git lists the main worktree first.
    let main = list_worktrees(start)?
        .into_iter()
        .find(|worktree| worktree.is_main)
        .ok_or_else(|| anyhow!("no main worktree found for {}", start.display()))?;
    Ok(std::fs::canonicalize(&main.path).unwrap_or(main.path))
}

pub fn list_branches(repo: &Path) -> Result<Vec<Branch>> {
    let local = run_git_lines(
        repo,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?;
    let remote = run_git_lines(
        repo,
        &["for-each-ref", "--format=%(refname:short)", "refs/remotes"],
    )?;

    let mut branches: Vec<Branch> = local
        .iter()
        .map(|name| Branch {
            display: name.clone(),
            local_name: name.clone(),
            local: true,
            remote: None,
        })
        .collect();

    for r in remote {
        if r.ends_with("/HEAD") {
            continue;
        }
        // `origin/feat/foo` -> local `feat/foo`
        let Some((remote_name, branch_part)) = r.split_once('/') else {
            continue;
        };
        let _ = remote_name;
        if branches.iter().any(|b| b.local_name == branch_part) {
            // already represented as a local branch; annotate it
            if let Some(b) = branches.iter_mut().find(|b| b.local_name == branch_part) {
                b.remote = Some(r.clone());
            }
            continue;
        }
        branches.push(Branch {
            display: r.clone(),
            local_name: branch_part.to_string(),
            local: false,
            remote: Some(r),
        });
    }
    branches.sort_by(|a, b| a.display.cmp(&b.display));
    Ok(branches)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeOutcome {
    Created,
    Existed,
}

#[derive(Debug, Clone)]
pub struct WorktreeRef {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub is_main: bool,
    pub prunable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStatus {
    Merged,
    Unmerged,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct WorktreeStatus {
    pub branch: Option<String>,
    pub dirty: bool,
    pub merge: MergeStatus,
}

pub fn list_worktrees(repo: &Path) -> Result<Vec<WorktreeRef>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .context("failed to run git worktree list")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git worktree list failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let mut refs: Vec<WorktreeRef> = Vec::new();
    let mut cur_path: Option<PathBuf> = None;
    let mut cur_branch: Option<String> = None;
    let mut cur_prunable = false;
    let mut first = true;

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.is_empty() {
            if let Some(path) = cur_path.take() {
                refs.push(WorktreeRef {
                    path,
                    branch: cur_branch.take(),
                    is_main: first,
                    prunable: cur_prunable,
                });
                first = false;
            }
            cur_branch = None;
            cur_prunable = false;
            continue;
        }
        if let Some(rest) = line.strip_prefix("worktree ") {
            cur_path = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("branch ") {
            cur_branch = Some(
                rest.trim()
                    .strip_prefix("refs/heads/")
                    .unwrap_or(rest.trim())
                    .to_string(),
            );
        } else if line.trim() == "prunable" || line.starts_with("prunable ") {
            cur_prunable = true;
        }
    }
    if let Some(path) = cur_path.take() {
        refs.push(WorktreeRef {
            path,
            branch: cur_branch,
            is_main: first,
            prunable: cur_prunable,
        });
    }
    Ok(refs)
}

pub fn find_worktree_for_branch(repo: &Path, branch: &str) -> Result<Option<WorktreeRef>> {
    Ok(list_worktrees(repo)?.into_iter().find(|worktree| {
        !worktree.prunable && worktree.path.exists() && worktree.branch.as_deref() == Some(branch)
    }))
}

pub fn inspect_worktree(worktree: &Path) -> Result<WorktreeStatus> {
    let status_output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["status", "--porcelain=v1", "--untracked-files=normal"])
        .output()
        .context("failed to run git status")?;
    if !status_output.status.success() {
        return Err(anyhow!(
            "git status failed in {}: {}",
            worktree.display(),
            String::from_utf8_lossy(&status_output.stderr).trim()
        ));
    }
    let dirty = !status_output.stdout.is_empty();

    let branch_output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .context("failed to run git symbolic-ref")?;
    let branch = branch_output.status.success().then(|| {
        String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string()
    });

    let remotes = run_git_lines(worktree, &["remote"])?;
    let mut default_refs = Vec::new();
    for remote in remotes {
        let remote_head = format!("refs/remotes/{remote}/HEAD");
        let output = Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(["symbolic-ref", "--quiet", &remote_head])
            .output()
            .with_context(|| format!("failed to resolve {remote_head}"))?;
        if output.status.success() {
            let reference = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !reference.is_empty() {
                default_refs.push(reference);
            }
        }
    }

    let merge = if default_refs.is_empty() {
        MergeStatus::Unknown
    } else {
        let mut merged = false;
        for reference in default_refs {
            let status = Command::new("git")
                .arg("-C")
                .arg(worktree)
                .args(["merge-base", "--is-ancestor", "HEAD", &reference])
                .status()
                .with_context(|| format!("failed to compare HEAD with {reference}"))?;
            match status.code() {
                Some(0) => {
                    merged = true;
                    break;
                }
                Some(1) => {}
                _ => {
                    return Err(anyhow!(
                        "git merge-base failed while comparing HEAD with {reference}"
                    ));
                }
            }
        }
        if merged {
            MergeStatus::Merged
        } else {
            MergeStatus::Unmerged
        }
    };

    Ok(WorktreeStatus {
        branch,
        dirty,
        merge,
    })
}

pub fn remove_worktree(repo: &Path, worktree: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "remove", "--force", "--force", "--"])
        .arg(worktree)
        .output()
        .context("failed to run git worktree remove")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git worktree remove failed for {}: {}",
            worktree.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

pub fn delete_local_branch(repo: &Path, branch: &str) -> Result<()> {
    let reference = format!("refs/heads/{branch}");
    let exists = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show-ref", "--verify", "--quiet", &reference])
        .output()
        .context("failed to check local branch")?;
    match exists.status.code() {
        Some(0) => {}
        Some(1) => return Ok(()),
        _ => {
            return Err(anyhow!(
                "git show-ref failed for {branch}: {}",
                String::from_utf8_lossy(&exists.stderr).trim()
            ));
        }
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["branch", "-D", "--", branch])
        .output()
        .context("failed to run git branch -D")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git branch -D failed for {branch}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

pub fn check_branch_name(repo: &Path, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["check-ref-format", "--branch", branch])
        .output()
        .context("failed to run git check-ref-format")?;
    if !output.status.success() {
        return Err(anyhow!(
            "invalid branch name {branch}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

pub fn rev_parse_commit(repo: &Path, rev: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", &format!("{rev}^{{commit}}")])
        .output()
        .context("failed to run git rev-parse")?;
    if !output.status.success() {
        return Err(anyhow!(
            "unknown commit {rev}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn default_remote(repo: &Path) -> Result<Option<String>> {
    let remotes = run_git_lines(repo, &["remote"])?;
    if remotes.iter().any(|remote| remote == "origin") {
        return Ok(Some("origin".to_string()));
    }
    Ok(remotes.into_iter().next())
}

pub fn current_branch(repo: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .output()
        .context("failed to run git symbolic-ref")?;
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || branch.is_empty() {
        return Err(anyhow!("cannot create a review from detached HEAD"));
    }
    Ok(branch)
}

pub fn remote_url(repo: &Path, remote: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["remote", "get-url", remote])
        .output()
        .context("failed to run git remote get-url")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git remote get-url failed for {remote}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn remote_push_url(repo: &Path, remote: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["remote", "get-url", "--push", remote])
        .output()
        .context("failed to run git remote get-url --push")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git remote get-url --push failed for {remote}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn head_oid(repo: &Path) -> Result<String> {
    rev_parse_commit(repo, "HEAD")
}

pub fn merge_base(repo: &Path, left: &str, right: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", left, right])
        .output()
        .context("failed to run git merge-base")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git merge-base failed for {left} and {right}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn default_remote_branch(repo: &Path, remote: &str) -> Result<String> {
    let reference = format!("refs/remotes/{remote}/HEAD");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["symbolic-ref", "--quiet", "--short", &reference])
        .output()
        .with_context(|| format!("failed to resolve {reference}"))?;
    let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let prefix = format!("{remote}/");
    if !output.status.success() || !resolved.starts_with(&prefix) {
        return Err(anyhow!(
            "could not determine the default branch for {remote}; pass --base"
        ));
    }
    Ok(resolved[prefix.len()..].to_string())
}

pub fn push_branch(repo_or_worktree: &Path, remote: &str, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_or_worktree)
        .args(["push", "-u", remote, branch])
        .output()
        .context("failed to run git push")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git push failed for {branch}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

pub fn remote_tracking_oid(repo: &Path, remote: &str, branch: &str) -> Result<Option<String>> {
    let reference = format!("refs/remotes/{remote}/{branch}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", &reference])
        .output()
        .with_context(|| format!("failed to resolve {reference}"))?;
    match output.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        )),
        Some(1) => Ok(None),
        _ => Err(anyhow!(
            "git rev-parse failed for {reference}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

/// Push `branch`, replacing the remote branch only if it still points at
/// `expected_remote_oid`. `None` means the remote branch must not exist yet.
pub fn force_push_branch(
    repo_or_worktree: &Path,
    remote: &str,
    branch: &str,
    expected_remote_oid: Option<&str>,
) -> Result<()> {
    let lease = format!(
        "--force-with-lease={branch}:{}",
        expected_remote_oid.unwrap_or_default()
    );
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_or_worktree)
        .args(["push", "-u", &lease, remote, branch])
        .output()
        .context("failed to run git push")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git push --force-with-lease failed for {branch}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

pub fn worktree_is_clean(worktree: &Path) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["status", "--porcelain"])
        .output()
        .context("failed to run git status")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git status failed in {}: {}",
            worktree.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout.is_empty())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRebaseStatus {
    Clean,
    Conflict,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRebase {
    pub branch: String,
    pub onto_oid: String,
    pub original_head_oid: String,
}

pub fn active_rebase(worktree: &Path) -> Result<Option<ActiveRebase>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .context("failed to resolve worktree git dir")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git rev-parse --absolute-git-dir failed in {}: {}",
            worktree.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let git_dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let directory = ["rebase-merge", "rebase-apply"]
        .into_iter()
        .map(|name| git_dir.join(name))
        .find(|path| path.is_dir());
    let Some(directory) = directory else {
        return Ok(None);
    };
    let read = |name: &str| -> Result<String> {
        let path = directory.join(name);
        std::fs::read_to_string(&path)
            .with_context(|| format!("read rebase metadata {}", path.display()))
            .map(|value| value.trim().to_string())
    };
    let head_name = read("head-name")?;
    let branch = head_name
        .strip_prefix("refs/heads/")
        .ok_or_else(|| anyhow!("active rebase has unexpected head name {head_name}"))?
        .to_string();
    Ok(Some(ActiveRebase {
        branch,
        onto_oid: read("onto")?,
        original_head_oid: read("orig-head")?,
    }))
}

pub fn rebase_onto(
    worktree: &Path,
    new_base_oid: &str,
    old_base_oid: &str,
) -> Result<GitRebaseStatus> {
    run_rebase_command(worktree, &["rebase", "--onto", new_base_oid, old_base_oid])
}

pub fn rebase_continue(worktree: &Path) -> Result<GitRebaseStatus> {
    run_rebase_command(worktree, &["rebase", "--continue"])
}

pub fn rebase_abort(worktree: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["rebase", "--abort"])
        .output()
        .context("failed to run git rebase --abort")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git rebase --abort failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

pub fn unmerged_files(worktree: &Path) -> Result<Vec<PathBuf>> {
    Ok(
        run_git_lines(worktree, &["diff", "--name-only", "--diff-filter=U"])?
            .into_iter()
            .map(PathBuf::from)
            .collect(),
    )
}

fn run_rebase_command(worktree: &Path, args: &[&str]) -> Result<GitRebaseStatus> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(args)
        .env("GIT_EDITOR", "true")
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if output.status.success() {
        return Ok(GitRebaseStatus::Clean);
    }
    if !unmerged_files(worktree)?.is_empty() {
        return Ok(GitRebaseStatus::Conflict);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Ok(GitRebaseStatus::Failed(if stderr.is_empty() {
        format!(
            "git {} exited with status {}",
            args.join(" "),
            output.status.code().unwrap_or(-1)
        )
    } else {
        stderr
    }))
}

/// Which files a new worktree checks out.
#[derive(Debug, Clone, Default)]
pub struct CheckoutScope {
    /// Directories to check out, as in `git sparse-checkout set --cone`.
    /// Empty means the whole tree.
    pub directories: Vec<String>,
    /// gitignore-style patterns that are never written to the worktree.
    pub exclude: Vec<String>,
}

/// The `git sparse-checkout set` arguments for `scope`, or `None` for a full
/// checkout. Exclusions need non-cone mode, so directories combined with
/// exclusions are written as the non-cone patterns that cone mode implies:
/// top-level files, the files directly inside each parent directory, and
/// everything inside each listed directory.
pub fn sparse_checkout_args(scope: &CheckoutScope) -> Result<Option<Vec<String>>> {
    let mut directories = Vec::new();
    for directory in &scope.directories {
        let trimmed = directory.trim_matches('/');
        if trimmed.is_empty() || trimmed.split('/').any(|part| part == ".." || part == ".") {
            return Err(anyhow!(
                "sparse directory must be a relative path inside the repository: {directory}"
            ));
        }
        directories.push(trimmed.to_string());
    }
    if directories.is_empty() && scope.exclude.is_empty() {
        return Ok(None);
    }
    if scope.exclude.is_empty() {
        let mut args = vec!["--cone".to_string()];
        args.extend(directories);
        return Ok(Some(args));
    }

    let mut patterns = vec!["--no-cone".to_string(), "/*".to_string()];
    if !directories.is_empty() {
        patterns.push("!/*/".to_string());
    }
    // Parent patterns come first so a later `!/parent/*/` cannot hide a
    // listed directory.
    for directory in &directories {
        let parts: Vec<&str> = directory.split('/').collect();
        for depth in 1..parts.len() {
            let parent = parts[..depth].join("/");
            for pattern in [format!("/{parent}/*"), format!("!/{parent}/*/")] {
                if !patterns.contains(&pattern) {
                    patterns.push(pattern);
                }
            }
        }
    }
    for directory in &directories {
        patterns.push(format!("/{directory}/"));
    }
    for pattern in &scope.exclude {
        patterns.push(format!("!{pattern}"));
    }
    Ok(Some(patterns))
}

pub fn create_worktree(
    repo: &Path,
    branch: &Branch,
    target: &Path,
    base: Option<&str>,
    scope: &CheckoutScope,
) -> Result<WorktreeOutcome> {
    if target.exists() {
        let canonical_target = target
            .canonicalize()
            .with_context(|| format!("resolve worktree target {}", target.display()))?;
        let existing = list_worktrees(repo)?.into_iter().find(|worktree| {
            !worktree.prunable
                && worktree.path.canonicalize().ok().as_ref() == Some(&canonical_target)
        });

        return match existing {
            Some(worktree) if worktree.branch.as_deref() == Some(&branch.local_name) => {
                Ok(WorktreeOutcome::Existed)
            }
            Some(worktree) => Err(anyhow!(
                "worktree target {} is registered for branch {}",
                target.display(),
                worktree.branch.as_deref().unwrap_or("<detached>")
            )),
            None => Err(anyhow!(
                "worktree target exists but is not a registered git worktree: {}",
                target.display()
            )),
        };
    }
    let sparse_args = sparse_checkout_args(scope)?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create worktree parent {}", parent.display()))?;
    }

    let mut args: Vec<String> = vec!["worktree".into(), "add".into()];
    if sparse_args.is_some() {
        args.push("--no-checkout".into());
    }
    if branch.local {
        args.push(target.display().to_string());
        args.push(branch.local_name.clone());
    } else if let Some(remote) = &branch.remote {
        args.push(target.display().to_string());
        args.extend(["-b".into(), branch.local_name.clone(), remote.clone()]);
    } else {
        // A new branch must not track its base (for example origin/main), or a
        // later `git push` would target the base branch.
        args.push("--no-track".into());
        args.push(target.display().to_string());
        args.extend(["-b".into(), branch.local_name.clone()]);
        // start-point; without one git forks off current HEAD
        if let Some(base) = base {
            args.push(base.to_string());
        }
    }
    let created_branch = (!branch.local).then_some(branch.local_name.as_str());

    let checkout = run_checkout_step(repo, &args).and_then(|()| {
        let Some(sparse_args) = &sparse_args else {
            return Ok(());
        };
        let mut set_args = vec!["sparse-checkout".to_string(), "set".to_string()];
        set_args.extend(sparse_args.iter().cloned());
        run_checkout_step(target, &set_args)?;
        run_checkout_step(target, &["checkout".to_string()])
    });
    if let Err(error) = checkout {
        remove_failed_worktree(repo, target, created_branch);
        let removed = match created_branch {
            Some(name) => format!("removed the partial worktree and branch {name}"),
            None => "removed the partial worktree".to_string(),
        };
        return Err(error.context(format!(
            "could not check out {}; {removed}",
            target.display()
        )));
    }
    Ok(WorktreeOutcome::Created)
}

fn run_checkout_step(directory: &Path, args: &[String]) -> Result<()> {
    // Keep stdout for `--print-path`; git's progress belongs on stderr.
    let status = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .stdout(std::io::stderr())
        .status()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !status.success() {
        return Err(anyhow!(
            "git {} failed (status {})",
            args.iter().take(2).cloned().collect::<Vec<_>>().join(" "),
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Undo a failed `create_worktree`, so a rerun does not find a half-created
/// branch or directory. `target` did not exist before, so it is safe to delete.
fn remove_failed_worktree(repo: &Path, target: &Path, created_branch: Option<&str>) {
    let _ = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "remove", "--force", "--force", "--"])
        .arg(target)
        .output();
    if target.exists() {
        let _ = std::fs::remove_dir_all(target);
    }
    let _ = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "prune"])
        .output();
    if let Some(name) = created_branch {
        let _ = delete_local_branch(repo, name);
    }
}

fn run_git_lines(repo: &Path, args: &[&str]) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
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

    fn init_repo(repo: &Path) {
        fs::create_dir_all(repo).unwrap();
        git(repo, &["init", "--initial-branch=main"]);
        git(repo, &["config", "user.email", "test@example.com"]);
        git(repo, &["config", "user.name", "Test User"]);
        fs::write(repo.join("README.md"), "main\n").unwrap();
        git(repo, &["add", "README.md"]);
        git(repo, &["commit", "-m", "initial"]);
    }

    fn new_branch(name: &str) -> Branch {
        Branch {
            display: name.to_string(),
            local_name: name.to_string(),
            local: false,
            remote: None,
        }
    }

    fn git_succeeds(path: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    }

    #[test]
    fn new_branch_from_remote_base_does_not_track_it() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_repo(&repo);
        let repo_arg = repo.to_string_lossy().to_string();
        git(&repo, &["remote", "add", "origin", &repo_arg]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let target = temp.path().join("worktrees").join("feature");

        let outcome = create_worktree(
            &repo,
            &new_branch("feature"),
            &target,
            Some("origin/main"),
            &CheckoutScope::default(),
        )
        .unwrap();

        assert_eq!(outcome, WorktreeOutcome::Created);
        assert_eq!(current_branch(&target).unwrap(), "feature");
        assert!(!git_succeeds(&repo, &["config", "branch.feature.merge"]));
        assert!(!git_succeeds(&repo, &["config", "branch.feature.remote"]));
    }

    fn files_in(worktree: &Path) -> Vec<String> {
        let mut files = Vec::new();
        let mut pending = vec![worktree.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).unwrap().flatten() {
                let path = entry.path();
                if path.file_name().is_some_and(|name| name == ".git") {
                    continue;
                }
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let relative = path.strip_prefix(worktree).unwrap();
                    files.push(relative.to_string_lossy().to_string());
                }
            }
        }
        files.sort();
        files
    }

    fn init_tree_repo(repo: &Path) {
        init_repo(repo);
        fs::create_dir_all(repo.join("packages/app/src")).unwrap();
        fs::create_dir_all(repo.join("tools")).unwrap();
        fs::write(repo.join("packages/app/src/main.ts"), "app\n").unwrap();
        fs::write(repo.join("packages/app/.dev.vars.example"), "KEY=\n").unwrap();
        fs::write(repo.join("packages/package.json"), "{}\n").unwrap();
        fs::write(repo.join("tools/build.sh"), "build\n").unwrap();
        fs::write(repo.join(".env.example"), "KEY=\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-m", "tree"]);
    }

    #[test]
    fn sparse_args_express_cone_directories_with_exclusions() {
        let cone = CheckoutScope {
            directories: vec!["packages/app/".into()],
            exclude: Vec::new(),
        };
        assert_eq!(
            sparse_checkout_args(&cone).unwrap().unwrap(),
            ["--cone", "packages/app"]
        );

        let combined = CheckoutScope {
            directories: vec!["packages/app".into(), "packages".into()],
            exclude: vec![".dev.vars*".into()],
        };
        assert_eq!(
            sparse_checkout_args(&combined).unwrap().unwrap(),
            [
                "--no-cone",
                "/*",
                "!/*/",
                "/packages/*",
                "!/packages/*/",
                "/packages/app/",
                "/packages/",
                "!.dev.vars*"
            ]
        );

        assert!(sparse_checkout_args(&CheckoutScope::default())
            .unwrap()
            .is_none());
        let escaping = CheckoutScope {
            directories: vec!["../outside".into()],
            exclude: Vec::new(),
        };
        assert!(sparse_checkout_args(&escaping).is_err());
    }

    #[test]
    fn checkout_exclude_and_sparse_directories_limit_written_files() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_tree_repo(&repo);

        let excluded = temp.path().join("excluded");
        let scope = CheckoutScope {
            directories: Vec::new(),
            exclude: vec![".dev.vars*".into(), ".env*".into()],
        };
        create_worktree(
            &repo,
            &new_branch("excluded"),
            &excluded,
            Some("main"),
            &scope,
        )
        .unwrap();
        assert_eq!(
            files_in(&excluded),
            [
                "README.md",
                "packages/app/src/main.ts",
                "packages/package.json",
                "tools/build.sh"
            ]
        );
        assert!(worktree_is_clean(&excluded).unwrap());

        let cone = temp.path().join("cone");
        let scope = CheckoutScope {
            directories: vec!["packages/app/src".into()],
            exclude: Vec::new(),
        };
        create_worktree(&repo, &new_branch("cone"), &cone, Some("main"), &scope).unwrap();
        assert_eq!(
            files_in(&cone),
            [
                ".env.example",
                "README.md",
                "packages/app/.dev.vars.example",
                "packages/app/src/main.ts",
                "packages/package.json"
            ]
        );

        let combined = temp.path().join("combined");
        let scope = CheckoutScope {
            directories: vec!["packages/app/src".into()],
            exclude: vec![".dev.vars*".into(), ".env*".into()],
        };
        create_worktree(
            &repo,
            &new_branch("combined"),
            &combined,
            Some("main"),
            &scope,
        )
        .unwrap();
        assert_eq!(
            files_in(&combined),
            [
                "README.md",
                "packages/app/src/main.ts",
                "packages/package.json"
            ]
        );
        assert!(worktree_is_clean(&combined).unwrap());
    }

    #[test]
    fn failed_checkout_removes_the_new_branch_and_directory() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_repo(&repo);
        fs::write(repo.join(".gitattributes"), "*.secret filter=blocked\n").unwrap();
        fs::write(repo.join("key.secret"), "secret\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "secret"]);
        // A required filter that fails stands in for a sandbox that refuses
        // to write a file.
        git(&repo, &["config", "filter.blocked.smudge", "false"]);
        git(&repo, &["config", "filter.blocked.required", "true"]);

        for scope in [
            CheckoutScope::default(),
            CheckoutScope {
                directories: Vec::new(),
                exclude: vec![".env*".into()],
            },
        ] {
            let target = temp.path().join("blocked");
            let error =
                create_worktree(&repo, &new_branch("blocked"), &target, Some("main"), &scope)
                    .unwrap_err();
            assert!(
                format!("{error:#}").contains("removed the partial worktree and branch blocked"),
                "{error:#}"
            );
            assert!(!target.exists());
            assert!(!git_succeeds(
                &repo,
                &["show-ref", "--verify", "--quiet", "refs/heads/blocked"]
            ));
            assert_eq!(list_worktrees(&repo).unwrap().len(), 1);
        }

        let target = temp.path().join("blocked");
        let scope = CheckoutScope {
            directories: Vec::new(),
            exclude: vec!["*.secret".into()],
        };
        let outcome =
            create_worktree(&repo, &new_branch("blocked"), &target, Some("main"), &scope).unwrap();
        assert_eq!(outcome, WorktreeOutcome::Created);
        assert!(!target.join("key.secret").exists());
    }

    #[test]
    fn force_push_replaces_only_the_expected_remote_branch() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let remote = temp.path().join("remote.git");
        init_repo(&repo);
        git(temp.path(), &["init", "--bare", "remote.git"]);
        let remote_arg = remote.to_string_lossy().to_string();
        git(&repo, &["remote", "add", "origin", &remote_arg]);
        git(&repo, &["switch", "-c", "feature"]);
        fs::write(repo.join("feature.txt"), "one\n").unwrap();
        git(&repo, &["add", "feature.txt"]);
        git(&repo, &["commit", "-m", "feature"]);

        assert_eq!(
            remote_tracking_oid(&repo, "origin", "feature").unwrap(),
            None
        );
        force_push_branch(&repo, "origin", "feature", None).unwrap();
        let pushed = remote_tracking_oid(&repo, "origin", "feature")
            .unwrap()
            .unwrap();
        assert_eq!(pushed, head_oid(&repo).unwrap());

        git(&repo, &["commit", "--amend", "-m", "rewritten feature"]);
        assert!(
            force_push_branch(&repo, "origin", "feature", Some(&head_oid(&repo).unwrap())).is_err()
        );
        assert!(force_push_branch(&repo, "origin", "feature", None).is_err());
        force_push_branch(&repo, "origin", "feature", Some(&pushed)).unwrap();
        assert_eq!(
            rev_parse_commit(&remote, "feature").unwrap(),
            head_oid(&repo).unwrap()
        );
    }

    #[test]
    fn inspects_dirty_and_unmerged_worktrees() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let worktree = temp.path().join("feature ");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--initial-branch=main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test User"]);
        fs::write(repo.join("README.md"), "main\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "initial"]);
        assert_eq!(current_branch(&repo).unwrap(), "main");
        let repo_arg = repo.to_string_lossy().to_string();
        git(&repo, &["remote", "add", "origin", &repo_arg]);
        assert_eq!(remote_url(&repo, "origin").unwrap(), repo_arg);
        assert_eq!(remote_push_url(&repo, "origin").unwrap(), repo_arg);
        assert_eq!(
            head_oid(&repo).unwrap(),
            rev_parse_commit(&repo, "HEAD").unwrap()
        );
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        assert_eq!(default_remote_branch(&repo, "origin").unwrap(), "main");
        let worktree_arg = worktree.to_string_lossy().to_string();
        git(
            &repo,
            &["worktree", "add", "-b", "feature/test", &worktree_arg],
        );

        let existing = find_worktree_for_branch(&repo, "feature/test")
            .unwrap()
            .unwrap();
        assert_eq!(
            existing.path.canonicalize().unwrap(),
            worktree.canonicalize().unwrap()
        );
        assert_eq!(common_dir(&repo).unwrap(), common_dir(&worktree).unwrap());
        let branch = Branch {
            display: "feature/test".to_string(),
            local_name: "feature/test".to_string(),
            local: true,
            remote: None,
        };
        assert_eq!(
            create_worktree(&repo, &branch, &worktree, None, &CheckoutScope::default()).unwrap(),
            WorktreeOutcome::Existed
        );

        let stale_target = temp.path().join("stale-target");
        fs::create_dir(&stale_target).unwrap();
        fs::write(stale_target.join("partial-checkout"), "stale\n").unwrap();
        let error = create_worktree(
            &repo,
            &branch,
            &stale_target,
            None,
            &CheckoutScope::default(),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("exists but is not a registered git worktree"));

        let clean = inspect_worktree(&worktree).unwrap();
        assert_eq!(current_branch(&worktree).unwrap(), "feature/test");
        assert_eq!(clean.branch.as_deref(), Some("feature/test"));
        assert!(!clean.dirty);
        assert_eq!(clean.merge, MergeStatus::Merged);

        fs::write(worktree.join("untracked.txt"), "work\n").unwrap();
        let dirty = inspect_worktree(&worktree).unwrap();
        assert!(dirty.dirty);
        assert_eq!(dirty.merge, MergeStatus::Merged);

        git(&worktree, &["add", "untracked.txt"]);
        git(&worktree, &["commit", "-m", "feature"]);
        let unmerged = inspect_worktree(&worktree).unwrap();
        assert!(!unmerged.dirty);
        assert_eq!(unmerged.merge, MergeStatus::Unmerged);

        git(&worktree, &["checkout", "--detach"]);
        assert!(current_branch(&worktree)
            .unwrap_err()
            .to_string()
            .contains("detached HEAD"));

        fs::write(worktree.join("dirty.txt"), "delete me\n").unwrap();
        remove_worktree(&repo, &worktree).unwrap();
        assert!(!worktree.exists());
        delete_local_branch(&repo, "feature/test").unwrap();
        let branch = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["show-ref", "--verify", "--quiet", "refs/heads/feature/test"])
            .status()
            .unwrap();
        assert!(!branch.success());
    }
}
