use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    #[serde(default)]
    pub recent: Vec<RecentDir>,
    #[serde(default)]
    pub worktrees: Vec<Worktree>,
    #[serde(default)]
    pub stack_branches: Vec<StackBranch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebase_progress: Option<RebaseProgress>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentDir {
    pub path: PathBuf,
    #[serde(with = "time::serde::rfc3339")]
    pub last_used: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worktree {
    pub path: PathBuf,
    pub repo: PathBuf,
    pub branch: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackBranch {
    pub repo: PathBuf,
    pub branch: String,
    pub base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_id: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebaseProgress {
    pub version: u32,
    pub repo: PathBuf,
    pub leaf: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_onto_oid: Option<String>,
    pub steps: Vec<RebaseStepState>,
    pub current: usize,
    #[serde(with = "time::serde::rfc3339")]
    pub started: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebaseStepState {
    pub branch: String,
    pub base: String,
    pub worktree: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_head_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_base_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_head_oid: Option<String>,
    pub status: RebaseStepStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RebaseStepStatus {
    Pending,
    Running,
    Conflict,
    Done,
}

pub fn state_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("SHIREI_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("shirei"));
        }
    }
    let home = dirs::home_dir().context("could not resolve home dir")?;
    Ok(home.join(".local").join("share").join("shirei"))
}

pub fn state_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("state.json"))
}

pub fn load() -> Result<State> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(State::default());
    }
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    if bytes.is_empty() {
        return Ok(State::default());
    }
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

pub fn save(state: &State) -> Result<()> {
    let path = state_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create state dir {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(state).context("serialize state")?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| format!("rename into {}", path.display()))?;
    Ok(())
}

impl State {
    pub fn touch_recent(&mut self, path: &Path, cap: usize) {
        self.recent.retain(|r| r.path != path);
        self.recent.insert(
            0,
            RecentDir {
                path: path.to_path_buf(),
                last_used: OffsetDateTime::now_utc(),
            },
        );
        if self.recent.len() > cap {
            self.recent.truncate(cap);
        }
    }

    pub fn add_worktree(&mut self, wt: Worktree) {
        self.worktrees.retain(|existing| {
            let same_path = path_eq(&existing.path, &wt.path);
            let same_branch = path_eq(&existing.repo, &wt.repo) && existing.branch == wt.branch;
            !(same_path || same_branch)
        });
        self.worktrees.push(wt);
    }

    pub fn upsert_stack_branch(&mut self, item: StackBranch) {
        self.stack_branches.retain(|existing| {
            !(path_eq(&existing.repo, &item.repo) && existing.branch == item.branch)
        });
        self.stack_branches.push(item);
    }

    pub fn untrack_stack_branch(&mut self, repo: &Path, branch: &str) -> bool {
        let before = self.stack_branches.len();
        self.stack_branches
            .retain(|item| !(path_eq(&item.repo, repo) && item.branch == branch));
        self.stack_branches.len() != before
    }

    pub fn stack_branch(&self, repo: &Path, branch: &str) -> Option<&StackBranch> {
        self.stack_branches
            .iter()
            .find(|item| path_eq(&item.repo, repo) && item.branch == branch)
    }

    pub fn stack_branch_mut(&mut self, repo: &Path, branch: &str) -> Option<&mut StackBranch> {
        self.stack_branches
            .iter_mut()
            .find(|item| path_eq(&item.repo, repo) && item.branch == branch)
    }

    pub fn worktree_for_branch(&self, repo: &Path, branch: &str) -> Option<&Worktree> {
        self.worktrees
            .iter()
            .find(|item| path_eq(&item.repo, repo) && item.branch == branch)
    }

    pub fn set_rebase_progress(&mut self, progress: RebaseProgress) {
        self.rebase_progress = Some(progress);
    }

    pub fn clear_rebase_progress(&mut self) {
        self.rebase_progress = None;
    }
}

fn path_eq(left: &Path, right: &Path) -> bool {
    let canonical_left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let canonical_right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    canonical_left == canonical_right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_state_defaults_stack_fields() {
        let state: State = serde_json::from_str(r#"{"recent":[],"worktrees":[]}"#).unwrap();
        assert!(state.stack_branches.is_empty());
        assert!(state.rebase_progress.is_none());
    }

    #[test]
    fn old_rebase_progress_defaults_oid_fields() {
        let state: State = serde_json::from_str(
            r#"{
                "recent": [],
                "worktrees": [],
                "rebase_progress": {
                    "version": 1,
                    "repo": "/repo",
                    "leaf": "feature",
                    "steps": [{
                        "branch": "feature",
                        "base": "main",
                        "worktree": "/worktree",
                        "status": "conflict"
                    }],
                    "current": 0,
                    "started": "2026-01-01T00:00:00Z",
                    "updated": "2026-01-01T00:00:00Z"
                }
            }"#,
        )
        .unwrap();
        let progress = state.rebase_progress.unwrap();
        assert!(progress.root_onto_oid.is_none());
        assert!(progress.steps[0].before_head_oid.is_none());
        assert!(progress.steps[0].old_base_oid.is_none());
        assert!(progress.steps[0].after_head_oid.is_none());
    }

    #[test]
    fn replaces_worktree_for_the_same_repo_and_branch() {
        let repo = PathBuf::from("/repo");
        let now = OffsetDateTime::now_utc();
        let mut state = State::default();
        state.add_worktree(Worktree {
            path: PathBuf::from("/old-worktree"),
            repo: repo.clone(),
            branch: "feature".into(),
            created: now,
        });
        state.add_worktree(Worktree {
            path: PathBuf::from("/new-worktree"),
            repo,
            branch: "feature".into(),
            created: now,
        });

        assert_eq!(state.worktrees.len(), 1);
        assert_eq!(state.worktrees[0].path, PathBuf::from("/new-worktree"));
    }

    #[test]
    fn upserts_stack_branches_by_repo_and_branch() {
        let repo = PathBuf::from("/repo");
        let now = OffsetDateTime::now_utc();
        let mut state = State::default();
        state.upsert_stack_branch(StackBranch {
            repo: repo.clone(),
            branch: "feature".into(),
            base: "main".into(),
            review_url: None,
            review_id: None,
            created: now,
            updated: now,
        });
        state.upsert_stack_branch(StackBranch {
            repo,
            branch: "feature".into(),
            base: "other".into(),
            review_url: None,
            review_id: None,
            created: now,
            updated: now,
        });
        assert_eq!(state.stack_branches.len(), 1);
        assert_eq!(state.stack_branches[0].base, "other");
    }
}
