use anyhow::{anyhow, Result};
use git2::{
    Cred, FetchOptions, PushOptions, RemoteCallbacks, Repository, StatusOptions,
    WorktreeAddOptions, WorktreePruneOptions,
};
use std::path::{Path, PathBuf};

pub fn open_repo(repo_root: &Path) -> Result<Repository> {
    Ok(Repository::open(repo_root)?)
}

pub fn is_dirty(repo: &Repository) -> Result<bool> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    Ok(!repo.statuses(Some(&mut opts))?.is_empty())
}

fn remote_callbacks() -> RemoteCallbacks<'static> {
    let mut cb = RemoteCallbacks::new();
    cb.credentials(|_url, username_from_url, allowed| {
        if allowed.is_ssh_key() {
            if let Some(u) = username_from_url {
                return Cred::ssh_key_from_agent(u);
            }
        }
        if allowed.is_user_pass_plaintext() {
            if let Ok(tok) = std::env::var("SHIREI_GIT_TOKEN") {
                let user = username_from_url.unwrap_or("x-access-token");
                return Cred::userpass_plaintext(user, &tok);
            }
        }
        Cred::default()
    });
    cb
}

pub fn fetch_origin(repo: &Repository) -> Result<()> {
    let mut remote = repo
        .find_remote("origin")
        .map_err(|_| anyhow!("no origin remote"))?;
    let mut fo = FetchOptions::new();
    fo.remote_callbacks(remote_callbacks());
    remote.fetch(&["refs/heads/*:refs/remotes/origin/*"], Some(&mut fo), None)?;
    Ok(())
}

pub fn create_or_update_branch_at_sha(repo: &Repository, branch: &str, sha: &str) -> Result<()> {
    let oid = git2::Oid::from_str(sha)?;
    let commit = repo.find_commit(oid)?;
    let refname = format!("refs/heads/{branch}");
    match repo.find_reference(&refname) {
        Ok(mut r) => {
            r.set_target(commit.id(), "shirei: move branch")?;
        }
        Err(_) => {
            repo.reference(&refname, commit.id(), true, "shirei: create branch")?;
        }
    }
    Ok(())
}

fn resolve_ref_to_commit(repo: &Repository, refname: &str) -> Result<git2::Oid> {
    let obj = repo.revparse_single(refname)?;
    Ok(obj.peel_to_commit()?.id())
}

pub fn branch_ahead_of_base(repo: &Repository, branch: &str, base_ref: &str) -> Result<bool> {
    let branch_ref = format!("refs/heads/{branch}");
    let branch_oid = resolve_ref_to_commit(repo, &branch_ref)?;
    let base_oid = resolve_ref_to_commit(repo, base_ref)?;
    let (ahead, _) = repo.graph_ahead_behind(branch_oid, base_oid)?;
    Ok(ahead > 0)
}

pub fn push_branch_origin(repo: &Repository, branch: &str) -> Result<()> {
    let mut remote = repo
        .find_remote("origin")
        .map_err(|_| anyhow!("no origin remote"))?;
    let mut opts = PushOptions::new();
    opts.remote_callbacks(remote_callbacks());
    let refspec = format!("refs/heads/{0}:refs/heads/{0}", branch);
    remote.push(&[&refspec], Some(&mut opts))?;
    Ok(())
}

pub fn create_worktree(
    repo: &Repository,
    worktree_name: &str,
    worktree_path: &Path,
    base_ref: &str,
    head_branch: Option<&str>,
) -> Result<PathBuf> {
    let obj = repo
        .revparse_single(base_ref)
        .map_err(|_| anyhow!("base_ref not found: {base_ref}"))?;
    let commit = obj
        .peel_to_commit()
        .map_err(|_| anyhow!("base_ref not commit-ish: {base_ref}"))?;

    let reference = if let Some(branch) = head_branch {
        let refname = format!("refs/heads/{branch}");
        let r = match repo.find_reference(&refname) {
            Ok(existing) => existing,
            Err(_) => repo.reference(
                &refname,
                commit.id(),
                true,
                "shirei: create branch for worktree",
            )?,
        };
        Some(r)
    } else {
        None
    };

    let mut opts = WorktreeAddOptions::new();
    if let Some(r) = reference.as_ref() {
        opts.reference(Some(r));
    }
    repo.worktree(worktree_name, worktree_path, Some(&opts))?;
    Ok(worktree_path.to_path_buf())
}

pub fn prune_worktree(repo: &Repository, worktree_name: &str) -> Result<()> {
    let wt = repo.find_worktree(worktree_name)?;
    wt.validate()?;
    let mut opts = WorktreePruneOptions::new();
    wt.prune(Some(&mut opts))?;
    Ok(())
}
