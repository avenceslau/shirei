use anyhow::{anyhow, Context, Result};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::config::{expand_tilde, Config};
use crate::rex::Placement;
use crate::session::{self, Backend, Session};
use crate::state::{StackBranch, State};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Session,
    Stack,
    Recent,
    Worktree,
    Root,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Section::Session => "SESSION",
            Section::Stack => "STACK",
            Section::Recent => "RECENT",
            Section::Worktree => "WORKTREE",
            Section::Root => "ROOT",
        }
    }

    fn ansi(self) -> &'static str {
        match self {
            Section::Session => "\x1b[36m",
            Section::Stack => "\x1b[35m",
            Section::Recent => "\x1b[33m",
            Section::Worktree => "\x1b[32m",
            Section::Root => "\x1b[37m",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Target {
    ActivateSession(String),
    OpenDir { path: PathBuf, name_hint: String },
}

#[derive(Debug, Clone)]
pub struct Row {
    pub section: Section,
    pub display: String,
    pub target: Target,
}

pub fn build_rows(state: &State, cfg: &Config, backend: Backend) -> Result<Vec<Row>> {
    let mut rows: Vec<Row> = Vec::new();
    let mut seen_session_names: HashSet<String> = HashSet::new();
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();

    // Split mode is a directory picker: live sessions would still switch Rex.app
    // and hide their directories from the later sections.
    let sessions = if matches!(backend, Backend::Rex) && cfg.rex_picker_split {
        Vec::new()
    } else {
        backend.list()?
    };
    for session in sessions {
        let Session { id, name, path, .. } = &session;
        rows.push(Row {
            section: Section::Session,
            display: format!("{name}  \x1b[90m{path}\x1b[0m"),
            target: Target::ActivateSession(id.clone()),
        });
        seen_session_names.insert(name.clone());
        if !path.is_empty() {
            seen_paths.insert(PathBuf::from(path));
        }
    }

    for item in &state.stack_branches {
        if let Some(worktree) = state.worktree_for_branch(&item.repo, &item.branch) {
            push_stack_row(
                &mut rows,
                &mut seen_session_names,
                &mut seen_paths,
                item,
                &worktree.path,
                backend.names_are_unique(),
            );
        }
    }

    for recent in &state.recent {
        if !push_dir_row(
            &mut rows,
            &mut seen_session_names,
            &mut seen_paths,
            Section::Recent,
            &recent.path,
            backend.names_are_unique(),
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
            backend.names_are_unique(),
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
                backend.names_are_unique(),
            );
        }
    }

    Ok(rows)
}

fn push_stack_row(
    rows: &mut Vec<Row>,
    seen_session_names: &mut HashSet<String>,
    seen_paths: &mut HashSet<PathBuf>,
    item: &StackBranch,
    path: &Path,
    deduplicate_names: bool,
) -> bool {
    if !path.is_dir() {
        return false;
    }
    if !seen_paths.insert(path.to_path_buf()) {
        return false;
    }
    let repo_slug = session::sanitize_name(
        &item
            .repo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_string()),
    );
    let branch_slug = session::sanitize_name(&item.branch);
    let name_hint = format!("{repo_slug}-{branch_slug}");
    if deduplicate_names && seen_session_names.contains(&name_hint) {
        return false;
    }
    rows.push(Row {
        section: Section::Stack,
        display: format!(
            "{}  \x1b[90mbase={} {}\x1b[0m",
            item.branch,
            item.base,
            path.display()
        ),
        target: Target::OpenDir {
            path: path.to_path_buf(),
            name_hint,
        },
    });
    true
}

fn push_dir_row(
    rows: &mut Vec<Row>,
    seen_session_names: &mut HashSet<String>,
    seen_paths: &mut HashSet<PathBuf>,
    section: Section,
    path: &Path,
    deduplicate_names: bool,
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
    let name_hint = session::sanitize_name(&basename);
    if deduplicate_names && seen_session_names.contains(&name_hint) {
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

pub struct Selection {
    pub index: usize,
    pub placement: Placement,
}

pub fn pick(rows: &[Row]) -> Result<Option<Selection>> {
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
            "--expect",
            "alt-enter,alt-t",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to launch fzf; install fzf to use the shirei picker")?;

    {
        let stdin = child.stdin.as_mut().context("failed to open fzf stdin")?;
        for (idx, row) in rows.iter().enumerate() {
            let written = writeln!(
                stdin,
                "{idx}\t{}{:<8}\x1b[0m {}",
                row.section.ansi(),
                row.section.label(),
                row.display
            );
            // fzf closes its input when it exits before reading every row, such as
            // when the picker is cancelled; its exit status decides the outcome.
            match written {
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => break,
                result => result?,
            }
        }
    }

    let output = child.wait_with_output().context("fzf did not complete")?;
    if !output.status.success() {
        return Ok(None);
    }
    let selected = String::from_utf8(output.stdout).context("fzf returned invalid utf-8")?;
    // --expect prints the accepting key (empty for Enter) on the line before the row.
    let mut lines = selected.lines();
    let placement = match lines.next() {
        Some("alt-enter") => Placement::SplitBelow,
        Some("alt-t") => Placement::Tab,
        _ => Placement::SplitRight,
    };
    let Some(first) = lines
        .next()
        .and_then(|row| row.split('\t').next())
        .map(str::trim)
    else {
        return Ok(None);
    };
    let Ok(idx) = first.parse::<usize>() else {
        return Err(anyhow!("fzf returned malformed row id: {first:?}"));
    };
    if idx >= rows.len() {
        return Err(anyhow!("fzf returned out-of-range row id: {idx}"));
    }
    Ok(Some(Selection {
        index: idx,
        placement,
    }))
}
