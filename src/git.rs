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

pub enum WorktreeOutcome {
    Created,
    Existed,
}

#[derive(Debug, Clone)]
pub struct WorktreeRef {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub is_main: bool,
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
                if !cur_prunable {
                    refs.push(WorktreeRef {
                        path,
                        branch: cur_branch.take(),
                        is_main: first,
                    });
                    first = false;
                }
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
        if !cur_prunable {
            refs.push(WorktreeRef {
                path,
                branch: cur_branch,
                is_main: first,
            });
        }
    }
    Ok(refs)
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
