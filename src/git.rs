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
            cur_path = Some(PathBuf::from(rest.trim()));
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

pub fn rebase(worktree: &Path, new_base: &str) -> Result<GitRebaseStatus> {
    run_rebase_command(worktree, &["rebase", new_base])
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

pub fn create_worktree(
    repo: &Path,
    branch: &Branch,
    target: &Path,
    base: Option<&str>,
) -> Result<WorktreeOutcome> {
    if target.exists() {
        return Ok(WorktreeOutcome::Existed);
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create worktree parent {}", parent.display()))?;
    }

    let args: Vec<String> = if branch.local {
        vec![
            "worktree".into(),
            "add".into(),
            target.display().to_string(),
            branch.local_name.clone(),
        ]
    } else if let Some(remote) = &branch.remote {
        vec![
            "worktree".into(),
            "add".into(),
            target.display().to_string(),
            "-b".into(),
            branch.local_name.clone(),
            remote.clone(),
        ]
    } else {
        let mut args = vec![
            "worktree".into(),
            "add".into(),
            target.display().to_string(),
            "-b".into(),
            branch.local_name.clone(),
        ];
        // start-point; without one git forks off current HEAD
        if let Some(base) = base {
            args.push(base.to_string());
        }
        args
    };

    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(&args)
        .status()
        .context("failed to run git worktree add")?;
    if !status.success() {
        return Err(anyhow!(
            "git worktree add failed (status {})",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(WorktreeOutcome::Created)
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

    #[test]
    fn inspects_dirty_and_unmerged_worktrees() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("repo");
        let worktree = temp.path().join("feature");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--initial-branch=main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test User"]);
        fs::write(repo.join("README.md"), "main\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "initial"]);
        let repo_arg = repo.to_string_lossy().to_string();
        git(&repo, &["remote", "add", "origin", &repo_arg]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let worktree_arg = worktree.to_string_lossy().to_string();
        git(
            &repo,
            &["worktree", "add", "-b", "feature/test", &worktree_arg],
        );

        let clean = inspect_worktree(&worktree).unwrap();
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
