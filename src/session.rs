use anyhow::Result;
use std::path::Path;

use crate::config::BackendKind;
use crate::{herdr, rex, tmux};

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub path: String,
    pub units: u32,
    pub attached: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum Backend {
    Tmux,
    Herdr,
    Rex,
}

impl From<BackendKind> for Backend {
    fn from(kind: BackendKind) -> Self {
        match kind {
            BackendKind::Tmux => Self::Tmux,
            BackendKind::Herdr => Self::Herdr,
            BackendKind::Rex => Self::Rex,
        }
    }
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Herdr => "herdr",
            Self::Rex => "rex",
        }
    }

    pub fn unit_name(self) -> &'static str {
        match self {
            Self::Tmux | Self::Rex => "windows",
            Self::Herdr => "tabs",
        }
    }

    pub fn names_are_unique(self) -> bool {
        matches!(self, Self::Tmux)
    }

    pub fn list(self) -> Result<Vec<Session>> {
        match self {
            Self::Tmux => tmux::list_sessions().map(|sessions| {
                sessions
                    .into_iter()
                    .map(|session| Session {
                        id: session.name.clone(),
                        name: session.name,
                        path: session.path,
                        units: session.windows,
                        attached: session.attached,
                    })
                    .collect()
            }),
            Self::Herdr => herdr::list_workspaces(),
            Self::Rex => rex::list_sessions(),
        }
    }

    pub fn current_id(self) -> Result<Option<String>> {
        match self {
            Self::Tmux => tmux::current_session_name(),
            Self::Herdr => Ok(std::env::var("HERDR_WORKSPACE_ID").ok()),
            Self::Rex => rex::current_session_id(),
        }
    }

    pub fn create(self, name: &str, path: &Path) -> Result<String> {
        match self {
            Self::Tmux => {
                tmux::new_detached(name, path)?;
                Ok(name.to_string())
            }
            Self::Herdr => {
                let label = path
                    .file_name()
                    .map(|component| component.to_string_lossy().into_owned())
                    .unwrap_or_else(|| name.to_string());
                herdr::create_workspace(&label, path)
            }
            Self::Rex => rex::create_session(name, path),
        }
    }

    pub fn activate(self, id: &str) -> Result<()> {
        match self {
            Self::Tmux => tmux::switch_or_attach(id),
            Self::Herdr => herdr::focus_or_attach(id),
            Self::Rex => rex::select_session(id),
        }
    }

    pub fn kill(self, id: &str) -> Result<()> {
        match self {
            Self::Tmux => tmux::kill_session(id),
            Self::Herdr => herdr::close_workspace(id),
            Self::Rex => rex::kill_session(id),
        }
    }

    pub fn run_setup(self, id: &str, path: &Path, cmd: &str) -> Result<()> {
        match self {
            Self::Tmux => tmux::run_in_new_window(id, "install", path, cmd),
            Self::Herdr => herdr::run_in_new_tab(id, path, cmd),
            Self::Rex => rex::run_in_new_window(id, path, cmd),
        }
    }
}

pub fn sanitize_name(hint: &str) -> String {
    tmux::sanitize_name(hint)
}
