use anyhow::{anyhow, Result};
use git2::{
    BranchType, Cred, FetchOptions, PushOptions, RemoteCallbacks, Repository, StatusOptions,
    WorktreeAddOptions, WorktreePruneOptions,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct BranchSummary {
    pub name: String,
    pub local_name: String,
    pub local: bool,
    pub remote_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchResolution {
    Local {
        branch: String,
        base_ref: String,
    },
    Remote {
        branch: String,
        base_ref: String,
        remote_name: String,
    },
    Missing,
    AmbiguousRemote {
        branch: String,
        candidates: Vec<String>,
    },
}

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

pub fn list_branches(repo: &Repository) -> Result<Vec<BranchSummary>> {
    let local_names = local_branch_names(repo)?;
    let mut remote_map = remote_branch_map(repo)?;
    let mut branches = Vec::new();

    for local_name in local_names {
        let remote_name = remote_map
            .remove(&local_name)
            .and_then(|candidates| preferred_remote_candidate(&candidates));
        branches.push(BranchSummary {
            name: local_name.clone(),
            local_name,
            local: true,
            remote_name,
        });
    }

    for (local_name, candidates) in remote_map {
        if candidates.len() == 1 {
            let remote_name = candidates.into_iter().next().expect("single candidate");
            branches.push(BranchSummary {
                name: local_name.clone(),
                local_name,
                local: false,
                remote_name: Some(remote_name),
            });
            continue;
        }

        for remote_name in candidates {
            branches.push(BranchSummary {
                name: remote_name.clone(),
                local_name: local_name.clone(),
                local: false,
                remote_name: Some(remote_name),
            });
        }
    }

    branches.sort_by(|left, right| {
        right
            .local
            .cmp(&left.local)
            .then_with(|| left.local_name.cmp(&right.local_name))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(branches)
}

pub fn resolve_branch(repo: &Repository, branch_name: &str) -> Result<BranchResolution> {
    let branch_name = branch_name.trim();
    if branch_name.is_empty() {
        return Ok(BranchResolution::Missing);
    }

    let local_ref = format!("refs/heads/{branch_name}");
    if repo.find_reference(&local_ref).is_ok() {
        return Ok(BranchResolution::Local {
            branch: branch_name.to_string(),
            base_ref: local_ref,
        });
    }

    let explicit_remote_ref = format!("refs/remotes/{branch_name}");
    if repo.find_reference(&explicit_remote_ref).is_ok() {
        if let Some((_, short_name)) = remote_branch_parts(branch_name) {
            return Ok(BranchResolution::Remote {
                branch: short_name.to_string(),
                base_ref: explicit_remote_ref,
                remote_name: branch_name.to_string(),
            });
        }
    }

    let remote_map = remote_branch_map(repo)?;
    let Some(candidates) = remote_map.get(branch_name) else {
        return Ok(BranchResolution::Missing);
    };

    if let Some(origin_candidate) = candidates
        .iter()
        .find(|candidate| candidate.starts_with("origin/"))
    {
        return Ok(BranchResolution::Remote {
            branch: branch_name.to_string(),
            base_ref: format!("refs/remotes/{origin_candidate}"),
            remote_name: origin_candidate.clone(),
        });
    }

    if candidates.len() == 1 {
        let remote_name = candidates[0].clone();
        return Ok(BranchResolution::Remote {
            branch: branch_name.to_string(),
            base_ref: format!("refs/remotes/{remote_name}"),
            remote_name,
        });
    }

    Ok(BranchResolution::AmbiguousRemote {
        branch: branch_name.to_string(),
        candidates: candidates.clone(),
    })
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

fn local_branch_names(repo: &Repository) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for branch in repo.branches(Some(BranchType::Local))? {
        let (branch, _) = branch?;
        let Some(name) = branch.name()? else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        names.insert(name.to_string());
    }
    Ok(names)
}

fn remote_branch_map(repo: &Repository) -> Result<BTreeMap<String, Vec<String>>> {
    let mut remote_map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for branch in repo.branches(Some(BranchType::Remote))? {
        let (branch, _) = branch?;
        let Some(name) = branch.name()? else {
            continue;
        };
        let Some((_, short_name)) = remote_branch_parts(name) else {
            continue;
        };
        remote_map
            .entry(short_name.to_string())
            .or_default()
            .push(name.to_string());
    }

    for candidates in remote_map.values_mut() {
        candidates.sort();
        candidates.dedup();
    }

    Ok(remote_map)
}

fn preferred_remote_candidate(candidates: &[String]) -> Option<String> {
    candidates
        .iter()
        .find(|candidate| candidate.starts_with("origin/"))
        .cloned()
        .or_else(|| candidates.first().cloned())
}

fn remote_branch_parts(name: &str) -> Option<(&str, &str)> {
    let (remote, short_name) = name.split_once('/')?;
    if remote.is_empty() || short_name.is_empty() || short_name == "HEAD" {
        return None;
    }
    Some((remote, short_name))
}

#[cfg(test)]
mod tests {
    use super::{list_branches, resolve_branch, BranchResolution};
    use git2::{Repository, Signature};
    use tempfile::TempDir;

    fn init_repo() -> (TempDir, Repository) {
        let td = TempDir::new().expect("tempdir");
        let repo = Repository::init(td.path()).expect("init repo");
        let sig = Signature::now("Shirei Tests", "shirei@example.com").expect("signature");
        std::fs::write(td.path().join("README.md"), "hello\n").expect("write file");
        let mut index = repo.index().expect("index");
        index
            .add_path(std::path::Path::new("README.md"))
            .expect("add path");
        let tree_id = index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .expect("commit");
        drop(tree);
        (td, repo)
    }

    fn add_remote_ref(repo: &Repository, name: &str) {
        let head = repo.head().expect("head").target().expect("head target");
        repo.reference(
            &format!("refs/remotes/{name}"),
            head,
            true,
            "test remote ref",
        )
        .expect("create remote ref");
    }

    #[test]
    fn lists_local_and_remote_only_branches() {
        let (_td, repo) = init_repo();
        let head_commit = repo.head().expect("head").peel_to_commit().expect("commit");
        repo.branch("feature/local", &head_commit, true)
            .expect("local branch");
        add_remote_ref(&repo, "origin/feature/local");
        add_remote_ref(&repo, "origin/feature/remote-only");
        add_remote_ref(&repo, "upstream/feature/remote-only");

        let branches = list_branches(&repo).expect("branch list");
        assert!(branches.iter().any(|branch| {
            branch.name == "feature/local"
                && branch.local
                && branch.remote_name.as_deref() == Some("origin/feature/local")
        }));
        assert!(branches.iter().any(|branch| {
            branch.name == "origin/feature/remote-only"
                && !branch.local
                && branch.remote_name.as_deref() == Some("origin/feature/remote-only")
        }));
        assert!(branches.iter().any(|branch| {
            branch.name == "upstream/feature/remote-only"
                && !branch.local
                && branch.remote_name.as_deref() == Some("upstream/feature/remote-only")
        }));
    }

    #[test]
    fn resolves_local_and_remote_branches() {
        let (_td, repo) = init_repo();
        let head_commit = repo.head().expect("head").peel_to_commit().expect("commit");
        repo.branch("feature/local", &head_commit, true)
            .expect("local branch");
        add_remote_ref(&repo, "origin/feature/remote-only");

        assert_eq!(
            resolve_branch(&repo, "feature/local").expect("local resolution"),
            BranchResolution::Local {
                branch: "feature/local".to_string(),
                base_ref: "refs/heads/feature/local".to_string(),
            }
        );
        assert_eq!(
            resolve_branch(&repo, "feature/remote-only").expect("remote resolution"),
            BranchResolution::Remote {
                branch: "feature/remote-only".to_string(),
                base_ref: "refs/remotes/origin/feature/remote-only".to_string(),
                remote_name: "origin/feature/remote-only".to_string(),
            }
        );
        assert_eq!(
            resolve_branch(&repo, "origin/feature/remote-only").expect("explicit remote"),
            BranchResolution::Remote {
                branch: "feature/remote-only".to_string(),
                base_ref: "refs/remotes/origin/feature/remote-only".to_string(),
                remote_name: "origin/feature/remote-only".to_string(),
            }
        );
    }
}
