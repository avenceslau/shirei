use anyhow::Result;
use std::path::Path;

use crate::config::BackendKind;
use crate::{herdr, tmux};

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
}

impl From<BackendKind> for Backend {
    fn from(kind: BackendKind) -> Self {
        match kind {
            BackendKind::Tmux => Self::Tmux,
            BackendKind::Herdr => Self::Herdr,
        }
    }
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Tmux => "tmux",
            Self::Herdr => "herdr",
        }
    }

    pub fn unit_name(self) -> &'static str {
        match self {
            Self::Tmux => "windows",
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
        }
    }

    pub fn current_id(self) -> Result<Option<String>> {
        match self {
            Self::Tmux => tmux::current_session_name(),
            Self::Herdr => Ok(std::env::var("HERDR_WORKSPACE_ID").ok()),
        }
    }

    pub fn create(self, name: &str, path: &Path) -> Result<String> {
        match self {
            Self::Tmux => {
                tmux::new_detached(name, path)?;
                Ok(name.to_string())
            }
            Self::Herdr => herdr::create_workspace(name, path),
        }
    }

    pub fn activate(self, id: &str) -> Result<()> {
        match self {
            Self::Tmux => tmux::switch_or_attach(id),
            Self::Herdr => herdr::focus_or_attach(id),
        }
    }

    pub fn kill(self, id: &str) -> Result<()> {
        match self {
            Self::Tmux => tmux::kill_session(id),
            Self::Herdr => herdr::close_workspace(id),
        }
    }

    pub fn run_setup(self, id: &str, path: &Path, cmd: &str) -> Result<()> {
        match self {
            Self::Tmux => tmux::run_in_new_window(id, "install", path, cmd),
            Self::Herdr => herdr::run_in_new_tab(id, path, cmd),
        }
    }
}

pub fn sanitize_name(hint: &str) -> String {
    tmux::sanitize_name(hint)
}
