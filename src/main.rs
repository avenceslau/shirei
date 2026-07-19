mod config;
mod forge;
mod git;
mod herdr;
mod install;
mod picker;
mod session;
mod state;
mod tmux;

use anyhow::{anyhow, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use std::path::Path;
use time::OffsetDateTime;

use crate::config::{expand_tilde, Config};
use crate::picker::{Row, Target};
use crate::session::Backend;
use crate::state::{State, Worktree};

#[derive(Parser, Debug)]
#[command(
    name = "shirei",
    version,
    about = "terminal session manager with worktree-aware dir tracking"
)]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Open the picker (default when no subcommand is given).
    Pick,
    /// Create or attach a git worktree for a branch and open a session.
    Branch {
        /// Branch name. If omitted, opens an fzf picker over local + remote branches.
        name: Option<String>,
        /// Start-point for a new branch. If omitted, opens an fzf picker to choose one.
        #[arg(short = 'b', long = "base")]
        base: Option<String>,
    },
    /// Print recent directories.
    Recent,
    /// Print live sessions known to the configured backend.
    Sessions,
    /// Select sessions to close.
    Kill,
    /// Generate a shell completion script.
    Completions {
        /// Shell to generate completions for.
        shell: Shell,
    },
    /// Manage tracked worktrees.
    Worktrees {
        #[command(subcommand)]
        action: Option<WorktreesCmd>,
    },
}

#[derive(Subcommand, Debug)]
enum WorktreesCmd {
    /// Print potentially unfinished worktrees without an active or merged PR or MR.
    Unfinished,
    /// Select tracked worktrees and permanently delete their directories and local branches.
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
        Cmd::Completions { shell } => {
            let mut command = Args::command();
            clap_complete::generate(shell, &mut command, "shirei", &mut std::io::stdout());
            Ok(())
        }
        Cmd::Worktrees { action } => match action {
            Some(WorktreesCmd::Unfinished) => run_worktrees_unfinished(),
            Some(WorktreesCmd::Prune) => run_worktrees_prune(),
            Some(WorktreesCmd::Scan) => run_worktrees_scan(),
            None => run_worktrees_list(),
        },
    }
}

fn run_picker() -> Result<()> {
    let cfg = config::load()?;
    let backend = Backend::from(cfg.backend);
    let mut state = state::load()?;
    let rows = picker::build_rows(&state, &cfg, backend)?;
    if rows.is_empty() {
        eprintln!("Nothing to show. Try `shirei branch <name>` to create a worktree session.");
        return Ok(());
    }
    let Some(idx) = picker::pick(&rows)? else {
        return Ok(());
    };
    apply_selection(&rows[idx], &cfg, backend, &mut state)
}

fn apply_selection(row: &Row, cfg: &Config, backend: Backend, state: &mut State) -> Result<()> {
    match &row.target {
        Target::ActivateSession(id) => {
            backend.activate(id)?;
        }
        Target::OpenDir { path, name_hint } => {
            open_session(backend, path, name_hint, None)?;
            state.touch_recent(path, cfg.recent_cap);
            state::save(state)?;
        }
    }
    Ok(())
}

fn open_session(
    backend: Backend,
    path: &Path,
    name_hint: &str,
    install_cmd: Option<&str>,
) -> Result<()> {
    let name = session::sanitize_name(name_hint);
    let existing = backend.list()?.into_iter().find(|session| {
        if backend.names_are_unique() {
            session.name == name
        } else {
            Path::new(&session.path) == path
        }
    });
    let id = if let Some(session) = existing {
        session.id
    } else {
        let id = backend.create(&name, path)?;
        if let Some(cmd) = install_cmd {
            backend.run_setup(&id, path, cmd)?;
        }
        id
    };
    backend.activate(&id)
}

fn run_branch(name: Option<&str>, base: Option<&str>) -> Result<()> {
    let cfg = config::load()?;
    let backend = Backend::from(cfg.backend);
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

    let repo_slug = session::sanitize_name(
        &repo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_string()),
    );
    let branch_slug = session::sanitize_name(&selected.local_name);
    let worktree_root = expand_tilde(&cfg.worktree_root);
    let target = worktree_root.join(&repo_slug).join(&branch_slug);

    let outcome = git::create_worktree(&repo, &selected, &target, start_point.as_deref())?;

    let install_cmd =
        if cfg.run_install_on_create && matches!(outcome, git::WorktreeOutcome::Created) {
            install::detect(&target)
        } else {
            None
        };

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
    open_session(backend, &target, &name_hint, install_cmd.as_deref())
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
    let cfg = config::load()?;
    let backend = Backend::from(cfg.backend);
    let sessions = backend.list()?;
    if sessions.is_empty() {
        println!("(no {} sessions)", backend.name());
        return Ok(());
    }
    for s in sessions {
        let attached = if s.attached { "*" } else { " " };
        println!(
            "{attached} {:<24} {}={:<3} {}",
            s.name,
            backend.unit_name(),
            s.units,
            s.path
        );
    }
    Ok(())
}

fn run_kill() -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let cfg = config::load()?;
    let backend = Backend::from(cfg.backend);
    let current = backend.current_id()?;
    let sessions: Vec<_> = backend
        .list()?
        .into_iter()
        .filter(|s| current.as_deref() != Some(s.id.as_str()))
        .collect();

    if sessions.is_empty() {
        if current.is_some() {
            println!(
                "(no {} sessions to kill; current session is excluded)",
                backend.name()
            );
        } else {
            println!("(no {} sessions)", backend.name());
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
                "{idx}\t{attached:<8} {}={:<3} {}  \x1b[90m{}\x1b[0m",
                backend.unit_name(),
                s.units,
                s.name,
                s.path
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
        selected.push(session.id.clone());
    }
    if selected.is_empty() {
        return Ok(());
    }

    for id in &selected {
        backend.kill(id)?;
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

fn run_worktrees_unfinished() -> Result<()> {
    let state = state::load()?;
    let mut worktrees = state.worktrees;
    worktrees.sort_by(|left, right| left.path.cmp(&right.path));

    let mut found = 0usize;
    for worktree in worktrees {
        if !worktree.path.is_dir() {
            println!(
                "{}\tbranch={}\treasons=missing\treview=unknown",
                worktree.path.display(),
                if worktree.branch.is_empty() {
                    "-"
                } else {
                    &worktree.branch
                }
            );
            found += 1;
            continue;
        }

        let status = match git::inspect_worktree(&worktree.path) {
            Ok(status) => status,
            Err(error) => {
                eprintln!("warn: {} -> {error}", worktree.path.display());
                println!(
                    "{}\tbranch={}\treasons=inspection-error\treview=unknown",
                    worktree.path.display(),
                    if worktree.branch.is_empty() {
                        "-"
                    } else {
                        &worktree.branch
                    }
                );
                found += 1;
                continue;
            }
        };

        let mut reasons = Vec::new();
        if status.dirty {
            reasons.push("dirty");
        }
        match status.merge {
            git::MergeStatus::Merged => {}
            git::MergeStatus::Unmerged => reasons.push("unmerged"),
            git::MergeStatus::Unknown if status.branch.is_some() => reasons.push("merge-unknown"),
            git::MergeStatus::Unknown => reasons.push("detached"),
        }
        if reasons.is_empty() {
            continue;
        }

        let (branch, review) = if let Some(branch) = status.branch {
            match forge::review_status(&worktree.path, &branch) {
                forge::ReviewStatus::Covered => continue,
                forge::ReviewStatus::Stale => (branch, "stale"),
                forge::ReviewStatus::NoneFound => (branch, "none"),
                forge::ReviewStatus::Unknown(errors) => {
                    for error in errors {
                        eprintln!("warn: {} -> {error}", worktree.path.display());
                    }
                    (branch, "unknown")
                }
            }
        } else {
            ("detached".to_string(), "unknown")
        };

        println!(
            "{}\tbranch={}\treasons={}\treview={}",
            worktree.path.display(),
            branch,
            reasons.join(","),
            review
        );
        found += 1;
    }

    if found == 0 {
        println!("(no potentially unfinished worktrees without active or merged reviews)");
    }
    Ok(())
}

fn run_worktrees_prune() -> Result<()> {
    use std::io::{self, Write};
    use std::process::{Command, Stdio};

    let mut state = state::load()?;
    if state.worktrees.is_empty() {
        println!("(no tracked worktrees)");
        return Ok(());
    }

    let cwd = std::env::current_dir().context("could not read cwd")?;
    let mut candidates: Vec<(Worktree, std::path::PathBuf, Option<String>, String)> = Vec::new();
    for worktree in &state.worktrees {
        let canonical_worktree =
            std::fs::canonicalize(&worktree.path).unwrap_or_else(|_| worktree.path.clone());
        let git_refs = git::list_worktrees(&worktree.repo).unwrap_or_default();
        let repo_path = git_refs
            .iter()
            .find(|item| item.is_main)
            .map(|item| item.path.clone())
            .unwrap_or_else(|| worktree.repo.clone());
        let git_ref = git_refs.into_iter().find(|item| {
            std::fs::canonicalize(&item.path).unwrap_or_else(|_| item.path.clone())
                == canonical_worktree
        });
        let mut labels = Vec::new();
        if !worktree.path.is_dir() {
            labels.push("MISSING");
        }
        if cwd.starts_with(&worktree.path) {
            labels.push("CURRENT");
        }
        if git_ref.as_ref().is_some_and(|item| item.is_main) {
            labels.push("MAIN");
        }
        if git_ref.as_ref().is_some_and(|item| item.prunable) {
            labels.push("PRUNABLE");
        }

        let mut branch = git_ref.as_ref().and_then(|item| item.branch.clone());
        if worktree.path.is_dir() {
            match git::inspect_worktree(&worktree.path) {
                Ok(status) => {
                    if status.dirty {
                        labels.push("DIRTY");
                    } else {
                        labels.push("CLEAN");
                    }
                    labels.push(match status.merge {
                        git::MergeStatus::Merged => "MERGED",
                        git::MergeStatus::Unmerged => "UNMERGED",
                        git::MergeStatus::Unknown => "MERGE-UNKNOWN",
                    });
                    if status.branch.is_none() {
                        labels.push("DETACHED");
                    }
                    branch = status.branch.or(branch);
                }
                Err(_) => labels.push("INSPECTION-ERROR"),
            }
        }
        if branch.is_none() && git_ref.is_none() && !worktree.branch.is_empty() {
            branch = Some(worktree.branch.clone());
        }
        candidates.push((worktree.clone(), repo_path, branch, labels.join(",")));
    }
    candidates.sort_by(|left, right| left.0.path.cmp(&right.0.path));

    let mut child = Command::new("fzf")
        .env_remove("FZF_DEFAULT_OPTS")
        .env_remove("FZF_DEFAULT_OPTS_FILE")
        .args([
            "--multi",
            "--prompt",
            "prune> ",
            "--header",
            "tab to select; enter reviews selection; type delete to confirm permanent removal",
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
        .context("failed to launch fzf for worktree pruning")?;

    {
        let stdin = child.stdin.as_mut().context("failed to open fzf stdin")?;
        for (idx, (worktree, _, branch, labels)) in candidates.iter().enumerate() {
            let branch_display: String = branch
                .as_deref()
                .unwrap_or("detached")
                .chars()
                .flat_map(char::escape_default)
                .collect();
            let path_display: String = worktree
                .path
                .to_string_lossy()
                .chars()
                .flat_map(char::escape_default)
                .collect();
            writeln!(
                stdin,
                "{idx}\t{labels:<32}\t{branch_display:<32}\t{path_display}"
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
        let candidate = candidates
            .get(idx)
            .ok_or_else(|| anyhow!("fzf returned out-of-range row id: {idx}"))?;
        selected.push(candidate.clone());
    }
    if selected.is_empty() {
        return Ok(());
    }

    eprintln!(
        "Permanently delete {} selected worktree(s), including local branches:",
        selected.len()
    );
    for (worktree, _, _, _) in &selected {
        let path_display: String = worktree
            .path
            .to_string_lossy()
            .chars()
            .flat_map(char::escape_default)
            .collect();
        eprintln!("  {path_display}");
    }
    eprint!("Type 'delete' to confirm: ");
    io::stderr().flush()?;
    let mut confirmation = String::new();
    io::stdin().read_line(&mut confirmation)?;
    if confirmation.trim() != "delete" {
        println!("cancelled");
        return Ok(());
    }

    let mut removed = 0usize;
    let mut failures = Vec::new();
    for (worktree, repo_path, fallback_branch, _) in selected {
        let git_refs = match git::list_worktrees(&repo_path) {
            Ok(refs) => refs,
            Err(error) => {
                failures.push(format!("{}: {error}", worktree.path.display()));
                continue;
            }
        };
        let canonical_worktree =
            std::fs::canonicalize(&worktree.path).unwrap_or_else(|_| worktree.path.clone());
        let git_ref = git_refs.into_iter().find(|item| {
            std::fs::canonicalize(&item.path).unwrap_or_else(|_| item.path.clone())
                == canonical_worktree
        });
        if git_ref.as_ref().is_some_and(|item| item.is_main) {
            failures.push(format!(
                "{}: refusing to remove the primary worktree",
                worktree.path.display()
            ));
            continue;
        }
        if let Some(item) = &git_ref {
            if let Err(error) = git::remove_worktree(&repo_path, &item.path) {
                failures.push(format!("{}: {error}", worktree.path.display()));
                continue;
            }
        } else if worktree.path.exists() {
            failures.push(format!(
                "{}: directory is not registered as a Git worktree",
                worktree.path.display()
            ));
            continue;
        }

        let branch = git_ref.and_then(|item| item.branch).or(fallback_branch);
        if let Some(branch) = branch {
            if let Err(error) = git::delete_local_branch(&repo_path, &branch) {
                if let Some(item) = state
                    .worktrees
                    .iter_mut()
                    .find(|item| item.path == worktree.path)
                {
                    item.repo = repo_path.clone();
                    item.branch = branch;
                }
                state.recent.retain(|item| item.path != worktree.path);
                state::save(&state)?;
                failures.push(format!("{}: {error}", worktree.path.display()));
                continue;
            }
        }

        state.worktrees.retain(|item| item.path != worktree.path);
        state.recent.retain(|item| item.path != worktree.path);
        state::save(&state)?;
        removed += 1;
    }

    println!("removed {removed} worktree(s)");
    if failures.is_empty() {
        Ok(())
    } else {
        for failure in &failures {
            eprintln!("warn: {failure}");
        }
        Err(anyhow!(
            "failed to fully remove {} worktree(s)",
            failures.len()
        ))
    }
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
