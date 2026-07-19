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
        self.worktrees.retain(|w| w.path != wt.path);
        self.worktrees.push(wt);
    }
}
