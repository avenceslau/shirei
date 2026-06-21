mod config;
mod git;
mod picker;
mod state;
mod tmux;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use std::path::Path;
use time::OffsetDateTime;

use crate::config::{expand_tilde, Config};
use crate::picker::{Row, Target};
use crate::state::{State, Worktree};

#[derive(Parser, Debug)]
#[command(
    name = "shirei",
    version,
    about = "tmux session manager with worktree-aware dir tracking"
)]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Open the picker (default when no subcommand is given).
    Pick,
    /// Create or attach a git worktree for a branch and open it in tmux.
    Branch {
        /// Branch name. If omitted, opens an fzf picker over local + remote branches.
        name: Option<String>,
        /// Start-point for a new branch. If omitted, opens an fzf picker to choose one.
        #[arg(short = 'b', long = "base")]
        base: Option<String>,
    },
    /// Print recent directories.
    Recent,
    /// Print live tmux sessions known to the server.
    Sessions,
    /// Select tmux sessions to close.
    Kill,
    /// Manage tracked worktrees.
    Worktrees {
        #[command(subcommand)]
        action: Option<WorktreesCmd>,
    },
}

#[derive(Subcommand, Debug)]
enum WorktreesCmd {
    /// Drop worktree entries whose path no longer exists.
    Prune,
    /// Walk configured roots and known repos, rebuild the worktree registry.
    Scan,
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.cmd.unwrap_or(Cmd::Pick) {
        Cmd::Pick => run_picker(),
        Cmd::Branch { name, base } => run_branch(name.as_deref(), base.as_deref()),
        Cmd::Recent => run_recent(),
        Cmd::Sessions => run_sessions(),
        Cmd::Kill => run_kill(),
        Cmd::Worktrees { action } => match action {
            Some(WorktreesCmd::Prune) => run_worktrees_prune(),
            Some(WorktreesCmd::Scan) => run_worktrees_scan(),
            None => run_worktrees_list(),
        },
    }
}

fn run_picker() -> Result<()> {
    let cfg = config::load()?;
    let mut state = state::load()?;
    let rows = picker::build_rows(&state, &cfg)?;
    if rows.is_empty() {
        eprintln!("Nothing to show. Try `shirei branch <name>` to create a worktree session.");
        return Ok(());
    }
    let Some(idx) = picker::pick(&rows)? else {
        return Ok(());
    };
    apply_selection(&rows[idx], &cfg, &mut state)
}

fn apply_selection(row: &Row, cfg: &Config, state: &mut State) -> Result<()> {
    match &row.target {
        Target::AttachSession(name) => {
            tmux::switch_or_attach(name)?;
        }
        Target::OpenDir { path, name_hint } => {
            open_in_tmux(path, name_hint)?;
            state.touch_recent(path, cfg.recent_cap);
            state::save(state)?;
        }
    }
    Ok(())
}

fn open_in_tmux(path: &Path, name_hint: &str) -> Result<()> {
    let session = tmux::sanitize_name(name_hint);
    if !tmux::has_session(&session) {
        tmux::new_detached(&session, path)?;
    }
    tmux::switch_or_attach(&session)
}

fn run_branch(name: Option<&str>, base: Option<&str>) -> Result<()> {
    let cfg = config::load()?;
    let mut state = state::load()?;

    let cwd = std::env::current_dir().context("could not read cwd")?;
    let repo = git::repo_root(&cwd)?;
    let branches = git::list_branches(&repo)?;

    let selected = match name.map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => {
            // Match against local_name, then display name.
            branches
                .iter()
                .find(|b| b.local_name == n)
                .or_else(|| branches.iter().find(|b| b.display == n))
                .cloned()
                .unwrap_or_else(|| git::Branch {
                    display: n.to_string(),
                    local_name: n.to_string(),
                    local: false,
                    remote: None,
                })
        }
        None => {
            if branches.is_empty() {
                return Err(anyhow!(
                    "no branches found in {}; pass a name to create one",
                    repo.display()
                ));
            }
            let Some(b) = pick_branch("branch> ", &branches)? else {
                return Ok(());
            };
            b
        }
    };

    let creating_new = !selected.local && selected.remote.is_none();
    let start_point = if creating_new {
        match base {
            Some(b) => Some(b.to_string()),
            None if branches.is_empty() => None,
            None => match pick_branch("base> ", &branches)? {
                Some(b) => Some(b.display),
                None => return Ok(()),
            },
        }
    } else {
        if base.is_some() {
            eprintln!("warn: --base ignored; {} already exists", selected.display);
        }
        None
    };

    let repo_slug = tmux::sanitize_name(
        &repo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_string()),
    );
    let branch_slug = tmux::sanitize_name(&selected.local_name);
    let worktree_root = expand_tilde(&cfg.worktree_root);
    let target = worktree_root.join(&repo_slug).join(&branch_slug);

    git::create_worktree(&repo, &selected, &target, start_point.as_deref())?;

    let now = OffsetDateTime::now_utc();
    state.add_worktree(Worktree {
        path: target.clone(),
        repo: repo.clone(),
        branch: selected.local_name.clone(),
        created: now,
    });
    state.touch_recent(&target, cfg.recent_cap);
    state::save(&state)?;

    let name_hint = format!("{repo_slug}-{branch_slug}");
    open_in_tmux(&target, &name_hint)
}

fn pick_branch(prompt: &str, branches: &[git::Branch]) -> Result<Option<git::Branch>> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("fzf")
        .args([
            "--ansi",
            "--prompt",
            prompt,
            "--layout",
            "reverse",
            "--height",
            "80%",
            "--delimiter",
            "\t",
            "--with-nth",
            "2..",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to launch fzf for branch picker")?;

    {
        let stdin = child.stdin.as_mut().context("failed to open fzf stdin")?;
        for (idx, b) in branches.iter().enumerate() {
            let kind = match (b.local, b.remote.is_some()) {
                (true, true) => "\x1b[32mLOCAL+REMOTE\x1b[0m",
                (true, false) => "\x1b[32mLOCAL\x1b[0m",
                (false, _) => "\x1b[36mREMOTE\x1b[0m",
            };
            writeln!(stdin, "{idx}\t{kind:<24} {}", b.display)?;
        }
    }

    let output = child.wait_with_output().context("fzf did not complete")?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8(output.stdout).context("fzf returned invalid utf-8")?;
    let Some(first) = stdout.split('\t').next().map(str::trim) else {
        return Ok(None);
    };
    let Ok(idx) = first.parse::<usize>() else {
        return Ok(None);
    };
    Ok(branches.get(idx).cloned())
}

fn run_recent() -> Result<()> {
    let state = state::load()?;
    if state.recent.is_empty() {
        println!("(no recent dirs)");
        return Ok(());
    }
    for r in &state.recent {
        println!("{}\t{}", r.last_used, r.path.display());
    }
    Ok(())
}

fn run_sessions() -> Result<()> {
    let sessions = tmux::list_sessions()?;
    if sessions.is_empty() {
        println!("(no tmux sessions)");
        return Ok(());
    }
    for s in sessions {
        let attached = if s.attached { "*" } else { " " };
        println!(
            "{attached} {:<24} windows={:<3} {}",
            s.name, s.windows, s.path
        );
    }
    Ok(())
}

fn run_kill() -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let current = tmux::current_session_name()?;
    let sessions: Vec<_> = tmux::list_sessions()?
        .into_iter()
        .filter(|s| current.as_deref() != Some(s.name.as_str()))
        .collect();

    if sessions.is_empty() {
        if current.is_some() {
            println!("(no tmux sessions to kill; current session is excluded)");
        } else {
            println!("(no tmux sessions)");
        }
        return Ok(());
    }

    let mut child = Command::new("fzf")
        .args([
            "--ansi",
            "--multi",
            "--prompt",
            "kill> ",
            "--header",
            "tab to select, enter to kill",
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
        .context("failed to launch fzf for session killer")?;

    {
        let stdin = child.stdin.as_mut().context("failed to open fzf stdin")?;
        for (idx, s) in sessions.iter().enumerate() {
            let attached = if s.attached { "ATTACHED" } else { "" };
            writeln!(
                stdin,
                "{idx}\t{attached:<8} windows={:<3} {}  \x1b[90m{}\x1b[0m",
                s.windows, s.name, s.path
            )?;
        }
    }

    let output = child.wait_with_output().context("fzf did not complete")?;
    if !output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8(output.stdout).context("fzf returned invalid utf-8")?;
    let mut selected = Vec::new();
    for line in stdout.lines() {
        let Some(first) = line.split('\t').next().map(str::trim) else {
            continue;
        };
        let idx = first
            .parse::<usize>()
            .map_err(|_| anyhow!("fzf returned malformed row id: {first:?}"))?;
        let session = sessions
            .get(idx)
            .ok_or_else(|| anyhow!("fzf returned out-of-range row id: {idx}"))?;
        selected.push(session.name.clone());
    }
    if selected.is_empty() {
        return Ok(());
    }

    for name in &selected {
        tmux::kill_session(name)?;
    }
    println!("killed {} session(s)", selected.len());
    Ok(())
}

fn run_worktrees_list() -> Result<()> {
    let state = state::load()?;
    if state.worktrees.is_empty() {
        println!("(no tracked worktrees)");
        return Ok(());
    }
    for w in &state.worktrees {
        let alive = if w.path.is_dir() { " " } else { "!" };
        let branch = if w.branch.is_empty() { "-" } else { &w.branch };
        println!(
            "{alive} {}\tbranch={}\trepo={}",
            w.path.display(),
            branch,
            w.repo.display()
        );
    }
    Ok(())
}

fn run_worktrees_prune() -> Result<()> {
    let mut state = state::load()?;
    let dropped = state.prune_dead_worktrees();
    state::save(&state)?;
    println!("pruned {dropped} dead worktree entries");
    Ok(())
}

fn run_worktrees_scan() -> Result<()> {
    use std::collections::{BTreeMap, BTreeSet};

    let cfg = config::load()?;
    let mut state = state::load()?;

    let mut repos: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    for w in &state.worktrees {
        repos.insert(w.repo.clone());
    }
    for root in &cfg.roots {
        if let Some(parent_str) = root.strip_suffix("/*") {
            let parent = expand_tilde(parent_str);
            if let Ok(entries) = std::fs::read_dir(&parent) {
                for e in entries.flatten() {
                    let path = e.path();
                    if path.join(".git").exists() {
                        repos.insert(path);
                    }
                }
            }
        } else {
            let path = expand_tilde(root);
            if path.join(".git").exists() {
                repos.insert(path);
            }
        }
    }

    let excludes: Vec<std::path::PathBuf> = cfg
        .worktree_exclude_prefixes
        .iter()
        .map(|p| expand_tilde(p))
        .collect();

    let previous: BTreeMap<std::path::PathBuf, Worktree> = state
        .worktrees
        .iter()
        .map(|w| (w.path.clone(), w.clone()))
        .collect();

    let mut by_path: BTreeMap<std::path::PathBuf, Worktree> = BTreeMap::new();
    let mut errors: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut excluded = 0usize;
    for repo in &repos {
        match git::list_worktrees(repo) {
            Ok(refs) => {
                for r in refs {
                    if r.is_main || !r.path.is_dir() {
                        continue;
                    }
                    if excludes.iter().any(|e| r.path.starts_with(e)) {
                        excluded += 1;
                        continue;
                    }
                    if by_path.contains_key(&r.path) {
                        continue;
                    }
                    let created = previous
                        .get(&r.path)
                        .map(|w| w.created)
                        .or_else(|| {
                            std::fs::metadata(&r.path)
                                .ok()
                                .and_then(|m| m.created().ok())
                                .map(OffsetDateTime::from)
                        })
                        .unwrap_or_else(OffsetDateTime::now_utc);
                    by_path.insert(
                        r.path.clone(),
                        Worktree {
                            path: r.path,
                            repo: repo.clone(),
                            branch: r.branch.unwrap_or_default(),
                            created,
                        },
                    );
                }
            }
            Err(e) => errors.push((repo.clone(), e.to_string())),
        }
    }

    state.worktrees = by_path.into_values().collect();
    state::save(&state)?;

    println!(
        "scanned {} repo(s), indexed {} linked worktree(s) (excluded {} by prefix)",
        repos.len(),
        state.worktrees.len(),
        excluded
    );
    for (repo, err) in &errors {
        eprintln!("warn: {} -> {}", repo.display(), err);
    }
    Ok(())
}
