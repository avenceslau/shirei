mod config;
mod forge;
mod git;
mod herdr;
mod install;
mod picker;
mod session;
mod stack;
mod state;
mod tmux;

use anyhow::{anyhow, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

use crate::config::{expand_tilde, Config};
use crate::picker::{Row, Target};
use crate::session::Backend;
use crate::state::{
    RebaseProgress, RebaseStepState, RebaseStepStatus, StackBranch, State, Worktree,
};

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
    /// Manage stacked PR branches, rebases, and review submission.
    Stack {
        #[command(subcommand)]
        action: StackCmd,
    },
}

#[derive(Subcommand, Debug)]
enum StackCmd {
    /// Create or attach to a branch worktree and record its PR base.
    Branch {
        /// Branch name to create or attach.
        name: String,
        /// PR/MR base branch for this stack entry.
        #[arg(short = 'b', long = "base")]
        base: String,
    },
    /// Record stack metadata for an existing branch.
    Track {
        branch: String,
        #[arg(short = 'b', long = "base")]
        base: String,
        #[arg(long = "repo")]
        repo: Option<PathBuf>,
    },
    /// Remove stack metadata without deleting worktrees or branches.
    Untrack {
        branch: String,
        #[arg(long = "repo")]
        repo: Option<PathBuf>,
    },
    /// Print stack branches in dependency order.
    List {
        #[arg(long = "repo")]
        repo: Option<PathBuf>,
        #[arg(long = "all")]
        all: bool,
        #[arg(long = "porcelain")]
        porcelain: bool,
    },
    /// Cascade rebase a stack from root to leaf.
    Rebase {
        #[arg(long = "branch")]
        branch: Option<String>,
        #[arg(long = "repo")]
        repo: Option<PathBuf>,
        #[arg(long = "onto")]
        onto: Option<String>,
        #[arg(long = "dry-run")]
        dry_run: bool,
        #[arg(long = "continue")]
        continue_: bool,
        #[arg(long = "abort")]
        abort: bool,
    },
    /// Push branches and create or update PRs/MRs.
    Submit {
        #[arg(long = "branch")]
        branch: Option<String>,
        #[arg(long = "repo")]
        repo: Option<PathBuf>,
        #[arg(long = "dry-run")]
        dry_run: bool,
        #[arg(long = "draft")]
        draft: bool,
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
        Cmd::Stack { action } => match action {
            StackCmd::Branch { name, base } => run_stack_branch(&name, &base),
            StackCmd::Track { branch, base, repo } => {
                run_stack_track(&branch, &base, repo.as_deref())
            }
            StackCmd::Untrack { branch, repo } => run_stack_untrack(&branch, repo.as_deref()),
            StackCmd::List {
                repo,
                all,
                porcelain,
            } => run_stack_list(repo.as_deref(), all, porcelain),
            StackCmd::Rebase {
                branch,
                repo,
                onto,
                dry_run,
                continue_,
                abort,
            } => run_stack_rebase(
                branch.as_deref(),
                repo.as_deref(),
                onto.as_deref(),
                dry_run,
                continue_,
                abort,
            ),
            StackCmd::Submit {
                branch,
                repo,
                dry_run,
                draft,
            } => run_stack_submit(branch.as_deref(), repo.as_deref(), dry_run, draft),
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

#[derive(Debug, Clone)]
struct BranchOpenResult {
    repo: PathBuf,
    branch: String,
    path: PathBuf,
    outcome: git::WorktreeOutcome,
}

fn run_branch(name: Option<&str>, base: Option<&str>) -> Result<()> {
    let cfg = config::load()?;
    let backend = Backend::from(cfg.backend);
    let mut state = state::load()?;
    let cwd = std::env::current_dir().context("could not read cwd")?;
    let repo = git::repo_root(&cwd)?;
    let result = match open_branch_worktree(&cfg, backend, &mut state, &repo, name, base, true) {
        Ok(result) => result,
        Err(error) if error.to_string().ends_with("selection cancelled") => return Ok(()),
        Err(error) => return Err(error),
    };
    state::save(&state)?;
    open_branch_result(backend, &result, &cfg)
}

fn open_branch_worktree(
    cfg: &Config,
    backend: Backend,
    state: &mut State,
    repo: &Path,
    name: Option<&str>,
    base: Option<&str>,
    allow_interactive: bool,
) -> Result<BranchOpenResult> {
    let branches = git::list_branches(repo)?;
    let selected = match name.map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => branches
            .iter()
            .find(|b| b.local_name == n)
            .or_else(|| branches.iter().find(|b| b.display == n))
            .cloned()
            .unwrap_or_else(|| git::Branch {
                display: n.to_string(),
                local_name: n.to_string(),
                local: false,
                remote: None,
            }),
        None if allow_interactive => {
            if branches.is_empty() {
                return Err(anyhow!(
                    "no branches found in {}; pass a name to create one",
                    repo.display()
                ));
            }
            let Some(b) = pick_branch("branch> ", &branches)? else {
                return Err(anyhow!("branch selection cancelled"));
            };
            b
        }
        None => return Err(anyhow!("branch name is required")),
    };

    let creating_new = !selected.local && selected.remote.is_none();
    let start_point = if creating_new {
        match base {
            Some(b) => Some(b.to_string()),
            None if branches.is_empty() => None,
            None if allow_interactive => match pick_branch("base> ", &branches)? {
                Some(b) => Some(b.display),
                None => return Err(anyhow!("base selection cancelled")),
            },
            None => return Err(anyhow!("--base is required for new stack branches")),
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
    let outcome = git::create_worktree(repo, &selected, &target, start_point.as_deref())?;

    state.add_worktree(Worktree {
        path: target.clone(),
        repo: repo.to_path_buf(),
        branch: selected.local_name.clone(),
        created: OffsetDateTime::now_utc(),
    });
    state.touch_recent(&target, cfg.recent_cap);

    let _ = backend;
    Ok(BranchOpenResult {
        repo: repo.to_path_buf(),
        branch: selected.local_name,
        path: target,
        outcome,
    })
}

fn open_branch_result(backend: Backend, result: &BranchOpenResult, cfg: &Config) -> Result<()> {
    let repo_slug = session::sanitize_name(
        &result
            .repo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_string()),
    );
    let branch_slug = session::sanitize_name(&result.branch);
    let install_cmd =
        if cfg.run_install_on_create && matches!(result.outcome, git::WorktreeOutcome::Created) {
            install::detect(&result.path)
        } else {
            None
        };
    let name_hint = format!("{repo_slug}-{branch_slug}");
    open_session(backend, &result.path, &name_hint, install_cmd.as_deref())
}

fn run_stack_branch(name: &str, base: &str) -> Result<()> {
    let cfg = config::load()?;
    let backend = Backend::from(cfg.backend);
    let mut state = state::load()?;
    let cwd = std::env::current_dir().context("could not read cwd")?;
    let repo = git::repo_root(&cwd)?;
    git::check_branch_name(&repo, name)?;
    git::rev_parse_commit(&repo, base)?;
    stack::validate_edge(&state, &repo, name, base)?;
    let result = open_branch_worktree(
        &cfg,
        backend,
        &mut state,
        &repo,
        Some(name),
        Some(base),
        false,
    )?;
    let now = OffsetDateTime::now_utc();
    let created = state
        .stack_branch(&repo, &result.branch)
        .map(|item| item.created)
        .unwrap_or(now);
    state.upsert_stack_branch(StackBranch {
        repo: repo.clone(),
        branch: result.branch.clone(),
        base: base.to_string(),
        review_url: state
            .stack_branch(&repo, &result.branch)
            .and_then(|item| item.review_url.clone()),
        review_id: state
            .stack_branch(&repo, &result.branch)
            .and_then(|item| item.review_id.clone()),
        created,
        updated: now,
    });
    state::save(&state)?;
    open_branch_result(backend, &result, &cfg)
}

fn run_stack_track(branch: &str, base: &str, repo_arg: Option<&Path>) -> Result<()> {
    let repo = resolve_repo(repo_arg)?;
    git::check_branch_name(&repo, branch)?;
    git::rev_parse_commit(&repo, branch)?;
    git::rev_parse_commit(&repo, base)?;
    let mut state = state::load()?;
    stack::validate_edge(&state, &repo, branch, base)?;
    let now = OffsetDateTime::now_utc();
    let existing = state.stack_branch(&repo, branch).cloned();
    state.upsert_stack_branch(StackBranch {
        repo: repo.clone(),
        branch: branch.to_string(),
        base: base.to_string(),
        review_url: existing.as_ref().and_then(|item| item.review_url.clone()),
        review_id: existing.as_ref().and_then(|item| item.review_id.clone()),
        created: existing.map(|item| item.created).unwrap_or(now),
        updated: now,
    });
    state::save(&state)?;
    println!("tracked {branch}\tbase={base}\trepo={}", repo.display());
    Ok(())
}

fn run_stack_untrack(branch: &str, repo_arg: Option<&Path>) -> Result<()> {
    let repo = resolve_repo(repo_arg)?;
    let mut state = state::load()?;
    let removed = state.untrack_stack_branch(&repo, branch);
    state::save(&state)?;
    if removed {
        println!("untracked {branch}\trepo={}", repo.display());
    } else {
        println!("(no stack metadata for {branch})");
    }
    Ok(())
}

fn run_stack_list(repo_arg: Option<&Path>, all: bool, porcelain: bool) -> Result<()> {
    let state = state::load()?;
    let repos: Vec<PathBuf> = if all {
        let mut repos: Vec<_> = state
            .stack_branches
            .iter()
            .map(|item| item.repo.clone())
            .collect();
        repos.sort();
        repos.dedup();
        repos
    } else {
        vec![resolve_repo(repo_arg)?]
    };
    if repos.is_empty() || state.stack_branches.is_empty() {
        println!("(no tracked stack branches)");
        return Ok(());
    }
    let mut printed = false;
    for repo in repos {
        let graph = stack::build_graph(&state, &repo)?;
        if graph.nodes.is_empty() {
            continue;
        }
        for line in stack::format_list(&graph, porcelain) {
            println!("{line}");
        }
        printed = true;
    }
    if !printed {
        println!("(no tracked stack branches)");
    }
    Ok(())
}

fn run_stack_rebase(
    branch: Option<&str>,
    repo_arg: Option<&Path>,
    onto: Option<&str>,
    dry_run: bool,
    continue_: bool,
    abort: bool,
) -> Result<()> {
    if [dry_run, continue_, abort]
        .iter()
        .filter(|value| **value)
        .count()
        > 1
    {
        return Err(anyhow!(
            "choose only one of --dry-run, --continue, or --abort"
        ));
    }
    let mut state = state::load()?;
    if abort {
        return run_stack_rebase_abort(&mut state);
    }
    if continue_ {
        return run_stack_rebase_continue(&mut state);
    }
    let repo = resolve_repo(repo_arg)?;
    let plan = stack::plan_rebase(&state, &repo, branch, onto)?;
    if dry_run {
        println!("repo={} leaf={}", plan.repo.display(), plan.leaf);
        for (idx, step) in plan.steps.iter().enumerate() {
            println!(
                "{}. {}\tworktree={}\trebase-onto={}",
                idx + 1,
                step.branch,
                step.worktree.display(),
                step.base
            );
        }
        return Ok(());
    }
    start_rebase_progress(&mut state, &plan)?;
    run_rebase_progress(&mut state)
}

fn run_stack_rebase_abort(state: &mut State) -> Result<()> {
    let Some(progress) = state.rebase_progress.clone() else {
        println!("(no stack rebase in progress)");
        return Ok(());
    };
    if progress.version != 1 {
        return Err(anyhow!(
            "unsupported stack rebase progress version {}",
            progress.version
        ));
    }
    if let Some(step) = progress.steps.get(progress.current) {
        if matches!(
            step.status,
            RebaseStepStatus::Conflict | RebaseStepStatus::Running
        ) {
            git::rebase_abort(&step.worktree)?;
        }
    }
    state.clear_rebase_progress();
    state::save(state)?;
    println!("aborted stack rebase for {}", progress.leaf);
    Ok(())
}

fn run_stack_rebase_continue(state: &mut State) -> Result<()> {
    let Some(progress) = state.rebase_progress.clone() else {
        return Err(anyhow!("no stack rebase in progress"));
    };
    if progress.version != 1 {
        return Err(anyhow!(
            "unsupported stack rebase progress version {}",
            progress.version
        ));
    }
    let Some(step) = progress.steps.get(progress.current) else {
        return Err(anyhow!("stack rebase progress is out of range"));
    };
    match git::rebase_continue(&step.worktree)? {
        git::GitRebaseStatus::Clean => {
            if let Some(progress) = state.rebase_progress.as_mut() {
                progress.steps[progress.current].status = RebaseStepStatus::Done;
                progress.current += 1;
                progress.updated = OffsetDateTime::now_utc();
            }
            state::save(state)?;
            run_rebase_progress(state)
        }
        git::GitRebaseStatus::Conflict => {
            let report = stack::find_conflicts(&step.worktree, &step.branch, &step.base)?;
            for line in stack::format_conflict_report(&report) {
                println!("{line}");
            }
            Ok(())
        }
        git::GitRebaseStatus::Failed(error) => Err(anyhow!(error)),
    }
}

fn start_rebase_progress(state: &mut State, plan: &stack::RebasePlan) -> Result<()> {
    if state.rebase_progress.is_some() {
        return Err(anyhow!(
            "a stack rebase is already in progress; continue or abort it first"
        ));
    }
    let now = OffsetDateTime::now_utc();
    state.set_rebase_progress(RebaseProgress {
        version: 1,
        repo: plan.repo.clone(),
        leaf: plan.leaf.clone(),
        steps: plan
            .steps
            .iter()
            .map(|step| RebaseStepState {
                branch: step.branch.clone(),
                base: step.base.clone(),
                worktree: step.worktree.clone(),
                status: RebaseStepStatus::Pending,
            })
            .collect(),
        current: 0,
        started: now,
        updated: now,
    });
    state::save(state)
}

fn run_rebase_progress(state: &mut State) -> Result<()> {
    loop {
        let Some(progress) = state.rebase_progress.clone() else {
            return Err(anyhow!("no stack rebase in progress"));
        };
        if progress.current >= progress.steps.len() {
            let leaf = progress.leaf;
            state.clear_rebase_progress();
            state::save(state)?;
            println!("completed stack rebase for {leaf}");
            return Ok(());
        }
        let step = progress.steps[progress.current].clone();
        if !git::worktree_is_clean(&step.worktree)? {
            return Err(anyhow!(
                "worktree is not clean before rebase: {}",
                step.worktree.display()
            ));
        }
        if let Some(progress) = state.rebase_progress.as_mut() {
            progress.steps[progress.current].status = RebaseStepStatus::Running;
            progress.updated = OffsetDateTime::now_utc();
        }
        state::save(state)?;
        match git::rebase(&step.worktree, &step.base)? {
            git::GitRebaseStatus::Clean => {
                if let Some(progress) = state.rebase_progress.as_mut() {
                    progress.steps[progress.current].status = RebaseStepStatus::Done;
                    progress.current += 1;
                    progress.updated = OffsetDateTime::now_utc();
                }
                state::save(state)?;
            }
            git::GitRebaseStatus::Conflict => {
                if let Some(progress) = state.rebase_progress.as_mut() {
                    progress.steps[progress.current].status = RebaseStepStatus::Conflict;
                    progress.updated = OffsetDateTime::now_utc();
                }
                state::save(state)?;
                let report = stack::find_conflicts(&step.worktree, &step.branch, &step.base)?;
                for line in stack::format_conflict_report(&report) {
                    println!("{line}");
                }
                return Ok(());
            }
            git::GitRebaseStatus::Failed(error) => return Err(anyhow!(error)),
        }
    }
}

fn run_stack_submit(
    branch: Option<&str>,
    repo_arg: Option<&Path>,
    dry_run: bool,
    draft: bool,
) -> Result<()> {
    let repo = resolve_repo(repo_arg)?;
    let mut state = state::load()?;
    let plan = stack::plan_submit(&state, &repo, branch)?;
    if dry_run {
        println!("repo={}", plan.repo.display());
        for step in &plan.steps {
            println!(
                "submit branch={} base={} worktree={} draft={draft}",
                step.branch,
                step.base,
                step.worktree
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "-".to_string())
            );
        }
        println!("description-links=append-or-replace-managed-block-only");
        return Ok(());
    }

    let remote = git::default_remote(&repo)?.ok_or_else(|| anyhow!("repository has no remotes"))?;
    let mut reviews = Vec::new();
    for step in &plan.steps {
        let worktree = step
            .worktree
            .as_ref()
            .ok_or_else(|| anyhow!("no tracked worktree for {}", step.branch))?;
        if !git::worktree_is_clean(worktree)? {
            return Err(anyhow!("worktree is dirty: {}", worktree.display()));
        }
        git::push_branch(worktree, &remote, &step.branch)?;
        let outcome = forge::ensure_review(forge::ReviewRequest {
            worktree,
            branch: &step.branch,
            base: &step.base,
            draft,
        })?;
        let now = OffsetDateTime::now_utc();
        if let Some(item) = state.stack_branch_mut(&repo, &step.branch) {
            item.review_id = outcome.id().map(str::to_string);
            item.review_url = outcome.url().map(str::to_string);
            item.updated = now;
        }
        reviews.push(stack::SubmittedReview {
            branch: step.branch.clone(),
            base: step.base.clone(),
            id: outcome.id().map(str::to_string),
            url: outcome.url().map(str::to_string),
        });
    }

    let links = stack::build_stack_links(&reviews)?;
    for review in &reviews {
        let Some(id) = review.id.as_deref() else {
            return Err(anyhow!("missing review id for {}", review.branch));
        };
        let step = plan
            .steps
            .iter()
            .find(|step| step.branch == review.branch)
            .ok_or_else(|| anyhow!("missing submit step for {}", review.branch))?;
        let worktree = step.worktree.as_ref().unwrap();
        let body = forge::get_review_body(worktree, id)?;
        let rendered = stack::render_stack_links(
            links
                .get(&review.branch)
                .ok_or_else(|| anyhow!("missing stack links for {}", review.branch))?,
        );
        let updated = stack::replace_stack_links_block(&body, &rendered)?;
        if updated != body {
            forge::update_review_body(worktree, id, &updated)?;
        }
    }
    state::save(&state)?;
    for review in reviews {
        println!(
            "submitted {}\tbase={}\turl={}",
            review.branch,
            review.base,
            review.url.unwrap_or_else(|| "-".to_string())
        );
    }
    Ok(())
}

fn resolve_repo(repo_arg: Option<&Path>) -> Result<PathBuf> {
    let start = match repo_arg {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().context("could not read cwd")?,
    };
    git::repo_root(&start)
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
        if let Some(branch_name) = branch.as_deref() {
            if let Err(error) = git::delete_local_branch(&repo_path, branch_name) {
                if let Some(item) = state
                    .worktrees
                    .iter_mut()
                    .find(|item| item.path == worktree.path)
                {
                    item.repo = repo_path.clone();
                    item.branch = branch_name.to_string();
                }
                state.recent.retain(|item| item.path != worktree.path);
                state::save(&state)?;
                failures.push(format!("{}: {error}", worktree.path.display()));
                continue;
            }
        }

        if let Some(branch_name) = branch.as_deref() {
            state.untrack_stack_branch(&repo_path, branch_name);
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
