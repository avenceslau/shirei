use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    #[default]
    Tmux,
    Herdr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub backend: BackendKind,
    #[serde(default = "default_roots")]
    pub roots: Vec<String>,
    #[serde(default = "default_recent_cap")]
    pub recent_cap: usize,
    #[serde(default = "default_worktree_root")]
    pub worktree_root: String,
    #[serde(default = "default_worktree_exclude_prefixes")]
    pub worktree_exclude_prefixes: Vec<String>,
    #[serde(default = "default_run_install_on_create")]
    pub run_install_on_create: bool,
    /// gitignore-style patterns that new worktrees never check out, through a
    /// non-cone sparse checkout.
    #[serde(default)]
    pub checkout_exclude: Vec<String>,
    #[serde(default)]
    pub review: ReviewConfig,
}

/// Rules `shirei pr create` applies to review titles and descriptions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewConfig {
    /// Forge hosts whose reviews are private. Reviews on any other host are
    /// public and are checked against the public denylist.
    pub private_hosts: Vec<String>,
    /// Terms added to the built-in public denylist.
    pub extra_public_denylist: Vec<String>,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            private_hosts: vec!["gitlab.corp.example".into()],
            extra_public_denylist: Vec::new(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: BackendKind::default(),
            roots: default_roots(),
            recent_cap: default_recent_cap(),
            worktree_root: default_worktree_root(),
            worktree_exclude_prefixes: default_worktree_exclude_prefixes(),
            run_install_on_create: default_run_install_on_create(),
            checkout_exclude: Vec::new(),
            review: ReviewConfig::default(),
        }
    }
}

fn default_roots() -> Vec<String> {
    vec!["~/projects/*".into(), "~".into(), "~/.config".into()]
}

fn default_recent_cap() -> usize {
    20
}

fn default_worktree_root() -> String {
    "~/.local/share/shirei/worktrees".into()
}

fn default_worktree_exclude_prefixes() -> Vec<String> {
    vec!["~/Library/Caches".into()]
}

fn default_run_install_on_create() -> bool {
    true
}

pub fn config_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("SHIREI_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("shirei").join("config.toml"));
        }
    }
    let home = dirs::home_dir().context("could not resolve home dir")?;
    Ok(home.join(".config").join("shirei").join("config.toml"))
}

pub fn load() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

pub fn expand_tilde(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    if input == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_defaults_to_tmux() {
        let config: Config = toml::from_str("").unwrap();
        assert!(matches!(config.backend, BackendKind::Tmux));
    }

    #[test]
    fn backend_accepts_herdr() {
        let config: Config = toml::from_str("backend = \"herdr\"").unwrap();
        assert!(matches!(config.backend, BackendKind::Herdr));
    }
}
