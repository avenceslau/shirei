use anyhow::{anyhow, Context, Result};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::{expand_tilde, Config};
use crate::state::State;
use crate::tmux::{self, TmuxSession};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Tmux,
    Recent,
    Worktree,
    Root,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Section::Tmux => "TMUX",
            Section::Recent => "RECENT",
            Section::Worktree => "WORKTREE",
            Section::Root => "ROOT",
        }
    }

    fn ansi(self) -> &'static str {
        match self {
            Section::Tmux => "\x1b[36m",
            Section::Recent => "\x1b[33m",
            Section::Worktree => "\x1b[32m",
            Section::Root => "\x1b[37m",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Target {
    AttachSession(String),
    OpenDir { path: PathBuf, name_hint: String },
}

#[derive(Debug, Clone)]
pub struct Row {
    pub section: Section,
    pub display: String,
    pub target: Target,
}

pub fn build_rows(state: &State, cfg: &Config) -> Result<Vec<Row>> {
    let mut rows: Vec<Row> = Vec::new();
    let mut seen_session_names: HashSet<String> = HashSet::new();
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();

    for session in tmux::list_sessions().unwrap_or_default() {
        let TmuxSession { name, path, .. } = &session;
        rows.push(Row {
            section: Section::Tmux,
            display: format!("{name}  \x1b[90m{path}\x1b[0m"),
            target: Target::AttachSession(name.clone()),
        });
        seen_session_names.insert(name.clone());
        if !path.is_empty() {
            seen_paths.insert(PathBuf::from(path));
        }
    }

    for recent in &state.recent {
        if !push_dir_row(
            &mut rows,
            &mut seen_session_names,
            &mut seen_paths,
            Section::Recent,
            &recent.path,
        ) {
            continue;
        }
    }

    for wt in &state.worktrees {
        push_dir_row(
            &mut rows,
            &mut seen_session_names,
            &mut seen_paths,
            Section::Worktree,
            &wt.path,
        );
    }

    for root in &cfg.roots {
        let paths = if let Some(parent_str) = root.strip_suffix("/*") {
            let parent = expand_tilde(parent_str);
            match std::fs::read_dir(&parent) {
                Ok(entries) => {
                    let mut out: Vec<PathBuf> = entries
                        .filter_map(|e| e.ok())
                        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                        .map(|e| e.path())
                        .collect();
                    out.sort();
                    out
                }
                Err(_) => Vec::new(),
            }
        } else {
            let path = expand_tilde(root);
            if path.is_dir() {
                vec![path]
            } else {
                Vec::new()
            }
        };
        for path in paths {
            push_dir_row(
                &mut rows,
                &mut seen_session_names,
                &mut seen_paths,
                Section::Root,
                &path,
            );
        }
    }

    Ok(rows)
}

fn push_dir_row(
    rows: &mut Vec<Row>,
    seen_session_names: &mut HashSet<String>,
    seen_paths: &mut HashSet<PathBuf>,
    section: Section,
    path: &Path,
) -> bool {
    if !path.is_dir() {
        return false;
    }
    if !seen_paths.insert(path.to_path_buf()) {
        return false;
    }
    let basename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let name_hint = tmux::sanitize_name(&basename);
    if seen_session_names.contains(&name_hint) {
        // a tmux session already covers this path; skip
        return false;
    }
    rows.push(Row {
        section,
        display: format!("{basename}  \x1b[90m{}\x1b[0m", path.display()),
        target: Target::OpenDir {
            path: path.to_path_buf(),
            name_hint,
        },
    });
    true
}

pub fn pick(rows: &[Row]) -> Result<Option<usize>> {
    if rows.is_empty() {
        return Ok(None);
    }

    let mut child = Command::new("fzf")
        .args([
            "--ansi",
            "--prompt",
            "shirei> ",
            "--layout",
            "reverse",
            "--height",
            "80%",
            "--delimiter",
            "\t",
            "--with-nth",
            "2..",
            "--tiebreak",
            "begin,index",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to launch fzf; install fzf to use the shirei picker")?;

    {
        let stdin = child.stdin.as_mut().context("failed to open fzf stdin")?;
        for (idx, row) in rows.iter().enumerate() {
            writeln!(
                stdin,
                "{idx}\t{}{:<8}\x1b[0m {}",
                row.section.ansi(),
                row.section.label(),
                row.display
            )?;
        }
    }

    let output = child.wait_with_output().context("fzf did not complete")?;
    if !output.status.success() {
        return Ok(None);
    }
    let selected = String::from_utf8(output.stdout).context("fzf returned invalid utf-8")?;
    let Some(first) = selected.split('\t').next().map(str::trim) else {
        return Ok(None);
    };
    let Ok(idx) = first.parse::<usize>() else {
        return Err(anyhow!("fzf returned malformed row id: {first:?}"));
    };
    if idx >= rows.len() {
        return Err(anyhow!("fzf returned out-of-range row id: {idx}"));
    }
    Ok(Some(idx))
}
