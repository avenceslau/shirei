use anyhow::Result;
use std::path::{Path, PathBuf};

pub fn root(state_dir: &Path, project_id: &str) -> PathBuf {
    state_dir.join("worktrees").join(project_id)
}

pub fn alloc(root: &Path, name: &str) -> Result<PathBuf> {
    Ok(root.join(name))
}
