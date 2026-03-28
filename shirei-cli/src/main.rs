use anyhow::{anyhow, Context};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(name = "shirei", version, about = "Shirei CLI")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:7373")]
    base_url: String,
    #[arg(long)]
    token: Option<String>,
    #[arg(long)]
    state_dir: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    Setup {
        #[command(subcommand)]
        target: SetupCmd,
    },
    Start,
    Stop,
    Restart,
    Doctor {
        #[arg(long)]
        project_id: Option<String>,
    },
    Health,
    Info,
    ProjectDiscover {
        path: String,
    },
    Projects,
    Sessions,
    Branch {
        branch_name: Option<String>,
        #[arg(long)]
        prompt: Option<String>,
    },
    Jobs {
        id: String,
        #[command(subcommand)]
        action: Option<JobsCmd>,
    },
    Spawn {
        project_id: String,
        name: String,
        prompt: Option<String>,
    },
    WatchStart {
        project_id: String,
        repo_key: String,
        pr_number: i64,
        #[arg(long)]
        retention_days: Option<i64>,
    },
    WatchList,
    WatchStop {
        watch_id: String,
    },
    Logs {
        job_id: String,
        #[arg(long)]
        tail_lines: Option<usize>,
    },
}

#[derive(Subcommand, Debug)]
enum JobsCmd {
    Attach,
    Edit,
    AttachStatus,
    AttachStop,
}

#[derive(Subcommand, Debug)]
enum SetupCmd {
    Opencode {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        print: bool,
    },
}

#[derive(Debug, Deserialize)]
struct JobApiResponse {
    job: JobRecord,
}

#[derive(Debug, Deserialize)]
struct JobRecord {
    job_id: String,
    name: String,
    cwd: String,
    worktree_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionsApiResponse {
    sessions: Vec<SessionRecord>,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionRecord {
    job_id: String,
    name: String,
    status: String,
    needs_attention: i64,
    cwd: String,
    worktree_name: Option<String>,
    worktree_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AttachApiResponse {
    attach: AttachRecord,
}

#[derive(Debug, Deserialize)]
struct AttachRecord {
    cwd: String,
    cmd: String,
    argv: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BranchesApiResponse {
    branches: Vec<BranchRecord>,
}

#[derive(Debug, Clone, Deserialize)]
struct BranchRecord {
    name: String,
    local_name: String,
    local: bool,
    remote_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProjectsApiResponse {
    projects: Vec<ProjectSummary>,
}

#[derive(Debug, Deserialize)]
struct ProjectSummary {
    project_id: String,
    git_remote: String,
    default_branch: String,
}

#[derive(Debug, Deserialize)]
struct DiscoverApiResponse {
    project: ProjectSummary,
}

#[derive(Debug, Deserialize)]
struct InfoApiResponse {
    providers: Option<ProviderInfo>,
    commands: Option<CommandsInfo>,
}

#[derive(Debug, Deserialize)]
struct ProviderInfo {
    github_configured: bool,
    gitlab_configured: bool,
}

#[derive(Debug, Deserialize)]
struct CommandsInfo {
    bootstrap_configured: Option<bool>,
    bootstrap_program: Option<String>,
    bootstrap_args: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct CliConfig {
    editor_command: Option<String>,
    editor: Option<EditorConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct EditorConfig {
    command: Option<String>,
}

impl CliConfig {
    fn editor_command(&self) -> Option<&str> {
        self.editor
            .as_ref()
            .and_then(|e| e.command.as_deref())
            .or(self.editor_command.as_deref())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a = Args::parse();
    let c = reqwest::Client::new();

    if a.cmd.is_none() {
        open_default_session_picker(&c, &a.base_url, a.token.as_deref(), a.state_dir.as_deref())
            .await?;
        return Ok(());
    }

    match a.cmd.expect("command checked") {
        Cmd::Setup { target } => match target {
            SetupCmd::Opencode { force, print } => setup_opencode_commands(force, print)?,
        },
        Cmd::Start => start_daemon(&c, &a.base_url, a.state_dir.as_deref()).await?,
        Cmd::Stop => stop_daemon(&a.base_url, a.state_dir.as_deref()).await?,
        Cmd::Restart => {
            stop_daemon(&a.base_url, a.state_dir.as_deref()).await?;
            start_daemon(&c, &a.base_url, a.state_dir.as_deref()).await?;
        }
        Cmd::Doctor { project_id } => {
            run_doctor(
                &c,
                &a.base_url,
                a.token.as_deref(),
                a.state_dir.as_deref(),
                project_id,
            )
            .await?;
        }
        Cmd::Health => {
            let v: serde_json::Value = send_api_request(&a.base_url, || {
                c.get(format!("{}/api/v0.1/health", a.base_url))
            })
            .await?
            .json()
            .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Info => {
            let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
            let v: serde_json::Value = send_api_request(&a.base_url, || {
                c.get(format!("{}/api/v0.1/info", a.base_url))
                    .header("Authorization", auth.clone())
            })
            .await?
            .json()
            .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::ProjectDiscover { path } => {
            let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
            let v: serde_json::Value = send_api_request(&a.base_url, || {
                c.post(format!("{}/api/v0.1/projects/discover", a.base_url))
                    .header("Authorization", auth.clone())
                    .json(&serde_json::json!({"root_path": path.clone()}))
            })
            .await?
            .json()
            .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Projects => {
            let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
            let v: serde_json::Value = send_api_request(&a.base_url, || {
                c.get(format!("{}/api/v0.1/projects", a.base_url))
                    .header("Authorization", auth.clone())
            })
            .await?
            .json()
            .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Sessions => {
            let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
            let sessions = fetch_active_sessions(&c, &a.base_url, &auth).await?;
            print_sessions(&sessions);
        }
        Cmd::Branch {
            branch_name,
            prompt,
        } => {
            create_or_attach_branch_session(
                &c,
                &a.base_url,
                a.token.as_deref(),
                a.state_dir.as_deref(),
                branch_name.as_deref(),
                prompt.as_deref(),
            )
            .await?;
        }
        Cmd::Jobs { id, action } => match action {
            Some(JobsCmd::Attach) => {
                let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
                attach_job(&c, &a.base_url, &auth, &id).await?
            }
            Some(JobsCmd::Edit) => {
                let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
                edit_job(&c, &a.base_url, &auth, &id).await?
            }
            Some(JobsCmd::AttachStatus) => {
                let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
                attach_status_job(&c, &a.base_url, &auth, &id).await?
            }
            Some(JobsCmd::AttachStop) => {
                let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
                attach_stop_job(&c, &a.base_url, &auth, &id).await?
            }
            None => {
                let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
                let v: serde_json::Value = send_api_request(&a.base_url, || {
                    c.get(format!("{}/api/v0.1/projects/{}/jobs", a.base_url, id))
                        .header("Authorization", auth.clone())
                })
                .await?
                .json()
                .await?;
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
        },
        Cmd::Spawn {
            project_id,
            name,
            prompt,
        } => {
            let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
            let v: serde_json::Value = send_api_request(&a.base_url, || {
                c.post(format!(
                    "{}/api/v0.1/projects/{}/jobs",
                    a.base_url, project_id
                ))
                .header("Authorization", auth.clone())
                .json(&serde_json::json!({"name": name.clone(), "prompt": prompt.clone()}))
            })
            .await?
            .json()
            .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::WatchStart {
            project_id,
            repo_key,
            pr_number,
            retention_days,
        } => {
            let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
            let v: serde_json::Value = send_api_request(&a.base_url, || {
                c.post(format!("{}/api/v0.1/watch-pr", a.base_url))
                    .header("Authorization", auth.clone())
                    .json(&serde_json::json!({"project_id": project_id.clone(), "repo_key": repo_key.clone(), "pr_number": pr_number, "retention_days": retention_days}))
            })
            .await?
            .json()
            .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::WatchList => {
            let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
            let v: serde_json::Value = send_api_request(&a.base_url, || {
                c.get(format!("{}/api/v0.1/watch-pr", a.base_url))
                    .header("Authorization", auth.clone())
            })
            .await?
            .json()
            .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::WatchStop { watch_id } => {
            let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
            let v: serde_json::Value = send_api_request(&a.base_url, || {
                c.post(format!(
                    "{}/api/v0.1/watch-pr/{}/stop",
                    a.base_url, watch_id
                ))
                .header("Authorization", auth.clone())
            })
            .await?
            .json()
            .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Logs { job_id, tail_lines } => {
            let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
            let url = if let Some(n) = tail_lines {
                format!(
                    "{}/api/v0.1/jobs/{}/logs?tail_lines={}",
                    a.base_url, job_id, n
                )
            } else {
                format!("{}/api/v0.1/jobs/{}/logs", a.base_url, job_id)
            };
            let v: serde_json::Value = send_api_request(&a.base_url, || {
                c.get(url.clone()).header("Authorization", auth.clone())
            })
            .await?
            .json()
            .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
    }

    Ok(())
}

async fn open_default_session_picker(
    client: &reqwest::Client,
    base_url: &str,
    token_arg: Option<&str>,
    state_dir_arg: Option<&str>,
) -> anyhow::Result<()> {
    let auth = authorization_header(token_arg, state_dir_arg)?;
    let sessions = fetch_active_sessions(client, base_url, &auth).await?;
    if sessions.is_empty() {
        println!("No active Shirei sessions. Create one with `shirei branch` or `shirei branch <branch-name>`.");
        return Ok(());
    }

    let Some(session) = pick_session_with_fzf(&sessions)? else {
        return Ok(());
    };

    open_job_in_tmux(
        base_url,
        token_arg,
        state_dir_arg,
        &session.job_id,
        session.worktree_path.as_deref().unwrap_or(&session.cwd),
        session.worktree_name.as_deref().unwrap_or(&session.job_id),
    )
}

async fn fetch_active_sessions(
    client: &reqwest::Client,
    base_url: &str,
    auth: &str,
) -> anyhow::Result<Vec<SessionRecord>> {
    let response: SessionsApiResponse = send_api_request(base_url, || {
        client
            .get(format!("{}/api/v0.1/sessions", base_url))
            .header("Authorization", auth)
    })
    .await?
    .error_for_status()?
    .json()
    .await
    .context("failed to list active sessions")?;
    Ok(response.sessions)
}

fn print_sessions(sessions: &[SessionRecord]) {
    if sessions.is_empty() {
        println!("No active Shirei sessions.");
        return;
    }

    for session in sessions {
        println!(
            "{:<8} {:<18} {}",
            session_badge(session),
            session.name,
            session.worktree_path.as_deref().unwrap_or(&session.cwd)
        );
    }
}

async fn create_or_attach_branch_session(
    client: &reqwest::Client,
    base_url: &str,
    token_arg: Option<&str>,
    state_dir_arg: Option<&str>,
    branch_name: Option<&str>,
    prompt: Option<&str>,
) -> anyhow::Result<()> {
    let auth = authorization_header(token_arg, state_dir_arg)?;
    let discovered = discover_current_project(client, base_url, &auth).await?;
    let project_id = discovered.project.project_id.clone();
    let selected_branch = match branch_name.map(str::trim).filter(|value| !value.is_empty()) {
        Some(branch_name) => branch_name.to_string(),
        None => {
            let branches = fetch_project_branches(client, base_url, &auth, &project_id).await?;
            if branches.is_empty() {
                println!("No local or remote branches found. Create one with `shirei branch <branch-name>`.");
                return Ok(());
            }

            let Some(branch) = pick_branch_with_fzf(&branches)? else {
                return Ok(());
            };
            branch.name
        }
    };

    let created: JobApiResponse = send_api_request(base_url, || {
        client
            .post(format!(
                "{}/api/v0.1/projects/{}/branches",
                base_url, project_id
            ))
            .header("Authorization", auth.clone())
            .json(&serde_json::json!({
                "branch_name": selected_branch.clone(),
                "prompt": prompt,
            }))
    })
    .await?
    .error_for_status()?
    .json()
    .await
    .with_context(|| format!("failed creating branch session for {}", selected_branch))?;

    if let Some(prompt) = prompt.filter(|value| !value.trim().is_empty()) {
        send_job_prompt(client, base_url, &auth, &created.job.job_id, prompt).await?;
    }

    let target_path = resolve_job_target_path(&created.job)?;
    open_job_in_tmux(
        base_url,
        token_arg,
        state_dir_arg,
        &created.job.job_id,
        &target_path.display().to_string(),
        &created.job.name,
    )
}

async fn fetch_project_branches(
    client: &reqwest::Client,
    base_url: &str,
    auth: &str,
    project_id: &str,
) -> anyhow::Result<Vec<BranchRecord>> {
    let response: BranchesApiResponse = send_api_request(base_url, || {
        client
            .get(format!(
                "{}/api/v0.1/projects/{}/branches",
                base_url, project_id
            ))
            .header("Authorization", auth)
    })
    .await?
    .error_for_status()?
    .json()
    .await
    .with_context(|| format!("failed listing branches for project {project_id}"))?;
    Ok(response.branches)
}

async fn send_job_prompt(
    client: &reqwest::Client,
    base_url: &str,
    auth: &str,
    job_id: &str,
    prompt: &str,
) -> anyhow::Result<()> {
    send_api_request(base_url, || {
        client
            .post(format!("{}/api/v0.1/jobs/{}/prompt", base_url, job_id))
            .header("Authorization", auth)
            .json(&serde_json::json!({"prompt": prompt}))
    })
    .await?
    .error_for_status()
    .with_context(|| format!("failed sending initial prompt to {job_id}"))?;
    Ok(())
}

async fn discover_current_project(
    client: &reqwest::Client,
    base_url: &str,
    auth: &str,
) -> anyhow::Result<DiscoverApiResponse> {
    let cwd = std::env::current_dir().context("failed resolving current directory")?;
    let cwd_str = cwd.display().to_string();
    send_api_request(base_url, || {
        client
            .post(format!("{}/api/v0.1/projects/discover", base_url))
            .header("Authorization", auth)
            .json(&serde_json::json!({"root_path": cwd_str.clone()}))
    })
    .await?
    .error_for_status()?
    .json()
    .await
    .context("failed to discover project for current directory")
}

fn pick_session_with_fzf(sessions: &[SessionRecord]) -> anyhow::Result<Option<SessionRecord>> {
    let mut child = std::process::Command::new("fzf")
        .args([
            "--prompt",
            "shirei> ",
            "--delimiter",
            "\t",
            "--with-nth",
            "2..",
            "--height",
            "80%",
            "--layout",
            "reverse",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to launch fzf; install fzf to use the default Shirei picker")?;

    {
        let stdin = child.stdin.as_mut().context("failed to open fzf stdin")?;
        for session in sessions {
            writeln!(
                stdin,
                "{}\t{}\t{}\t{}\t{}",
                session.job_id,
                session_badge(session),
                session.name,
                session.worktree_path.as_deref().unwrap_or(&session.cwd),
                session.status
            )?;
        }
    }

    let output = child.wait_with_output().context("fzf did not complete")?;
    if !output.status.success() {
        return Ok(None);
    }
    let selected = String::from_utf8(output.stdout).context("fzf returned invalid utf-8")?;
    let Some(job_id) = selected
        .split('\t')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    Ok(sessions.iter().find(|s| s.job_id == job_id).cloned())
}

fn pick_branch_with_fzf(branches: &[BranchRecord]) -> anyhow::Result<Option<BranchRecord>> {
    let mut child = std::process::Command::new("fzf")
        .args([
            "--prompt",
            "branch> ",
            "--delimiter",
            "\t",
            "--with-nth",
            "2..",
            "--height",
            "80%",
            "--layout",
            "reverse",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to launch fzf; install fzf to select a Shirei branch")?;

    {
        let stdin = child.stdin.as_mut().context("failed to open fzf stdin")?;
        for branch in branches {
            writeln!(
                stdin,
                "{}\t{}\t{}\t{}",
                branch.name,
                branch_kind(branch),
                branch.local_name,
                branch.remote_name.as_deref().unwrap_or("-")
            )?;
        }
    }

    let output = child.wait_with_output().context("fzf did not complete")?;
    if !output.status.success() {
        return Ok(None);
    }
    let selected = String::from_utf8(output.stdout).context("fzf returned invalid utf-8")?;
    let Some(branch_name) = selected
        .split('\t')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    Ok(branches
        .iter()
        .find(|branch| branch.name == branch_name)
        .cloned())
}

fn session_badge(session: &SessionRecord) -> &'static str {
    match session.status.as_str() {
        "AWAITING_REVIEW" | "DONE_UNREAD" => "REVIEW",
        _ if session.needs_attention > 0 => "INPUT",
        "RUNNING" => "RUNNING",
        "ERROR" => "ERROR",
        _ => "OPEN",
    }
}

fn branch_kind(branch: &BranchRecord) -> &'static str {
    match (branch.local, branch.remote_name.is_some()) {
        (true, true) => "LOCAL+REMOTE",
        (true, false) => "LOCAL",
        (false, _) => "REMOTE",
    }
}

fn open_job_in_tmux(
    base_url: &str,
    token: Option<&str>,
    state_dir: Option<&str>,
    job_id: &str,
    cwd: &str,
    name_hint: &str,
) -> anyhow::Result<()> {
    let session_name = tmux_session_name(name_hint, job_id);
    let current_exe = std::env::current_exe().context("failed resolving current shirei binary")?;

    let has_status = std::process::Command::new("tmux")
        .args(["has-session", "-t", &session_name])
        .status();
    let has_session = matches!(has_status, Ok(status) if status.success());

    if !has_session {
        let mut cmd = std::process::Command::new("tmux");
        cmd.arg("new-session")
            .arg("-d")
            .arg("-s")
            .arg(&session_name)
            .arg("-c")
            .arg(cwd)
            .arg(current_exe);
        cmd.arg("--base-url").arg(base_url);
        if let Some(token) = token {
            cmd.arg("--token").arg(token);
        } else if let Ok(token) = read_default_token(state_dir) {
            cmd.arg("--token").arg(token);
        }
        if let Some(dir) = state_dir {
            cmd.arg("--state-dir").arg(dir);
        }
        cmd.args(["jobs", job_id, "attach"]);

        let status = cmd
            .status()
            .context("failed to create tmux session; install tmux to open Shirei sessions")?;
        if !status.success() {
            return Err(anyhow!(
                "tmux new-session exited with status {}",
                status.code().unwrap_or(-1)
            ));
        }
    }

    let attach_action = if std::env::var_os("TMUX").is_some() {
        "switch-client"
    } else {
        "attach-session"
    };
    let status = std::process::Command::new("tmux")
        .args([attach_action, "-t", &session_name])
        .status()
        .context("failed to attach to tmux session")?;
    if !status.success() {
        return Err(anyhow!(
            "tmux {} exited with status {}",
            attach_action,
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

fn tmux_session_name(name_hint: &str, job_id: &str) -> String {
    let mut cleaned = name_hint
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    cleaned.truncate(28);
    cleaned = cleaned.trim_matches('-').to_string();
    let cleaned = if cleaned.is_empty() {
        "session"
    } else {
        &cleaned
    };
    let suffix = job_id.trim_start_matches("job_");
    let suffix = &suffix[..suffix.len().min(8)];
    format!("shirei-{cleaned}-{suffix}")
}

async fn send_api_request<F>(base_url: &str, request: F) -> anyhow::Result<reqwest::Response>
where
    F: FnMut() -> reqwest::RequestBuilder,
{
    send_api_request_with(base_url, request, || async { start_daemon_in_background() }).await
}

async fn send_api_request_with<F, B, BFut>(
    base_url: &str,
    mut request: F,
    mut bootstrap: B,
) -> anyhow::Result<reqwest::Response>
where
    F: FnMut() -> reqwest::RequestBuilder,
    B: FnMut() -> BFut,
    BFut: Future<Output = anyhow::Result<()>>,
{
    match request().send().await {
        Ok(response) => Ok(response),
        Err(err) if is_daemon_unreachable(&err) => {
            bootstrap()
                .await
                .context("failed to bootstrap shirei-daemon")?;
            wait_for_daemon_ready(base_url).await?;
            request()
                .send()
                .await
                .context("request failed after daemon bootstrap")
        }
        Err(err) => Err(err.into()),
    }
}

fn start_daemon_in_background() -> anyhow::Result<()> {
    std::process::Command::new("shirei-daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn shirei-daemon; install it or set SHIREI_DAEMON_BIN")?;
    Ok(())
}

async fn wait_for_daemon_ready(base_url: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let health_url = format!("{}/api/v0.1/health", base_url);
    for _ in 0..20 {
        match client.get(&health_url).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            Ok(_) => {}
            Err(err) if is_daemon_unreachable(&err) => {}
            Err(err) => return Err(err).context("daemon became reachable but health check failed"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!("daemon did not become healthy after bootstrap"))
}

fn is_daemon_unreachable(err: &reqwest::Error) -> bool {
    err.is_connect() || err.is_timeout()
}

async fn run_doctor(
    client: &reqwest::Client,
    base_url: &str,
    token_arg: Option<&str>,
    state_dir_arg: Option<&str>,
    project_filter: Option<String>,
) -> anyhow::Result<()> {
    let mut failures = 0_usize;
    let mut warnings = 0_usize;

    let mut record = |level: &str, name: &str, detail: String| {
        match level {
            "FAIL" => failures += 1,
            "WARN" => warnings += 1,
            _ => {}
        }
        println!("[{level}] {name}: {detail}");
    };

    println!("Shirei Doctor");

    let state = state_dir(state_dir_arg)?;
    record("OK", "state_dir", state.display().to_string());

    let token = match token_arg {
        Some(t) if !t.trim().is_empty() => {
            record("OK", "token", "provided via --token".to_string());
            Some(t.to_string())
        }
        _ => match read_default_token(state_dir_arg) {
            Ok(t) => {
                let path_hint = token_candidates(state_dir_arg)?
                    .into_iter()
                    .find(|p| p.exists())
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "token file".to_string());
                record("OK", "token", format!("loaded from {path_hint}"));
                Some(t)
            }
            Err(e) => {
                record("WARN", "token", format!("{e}"));
                None
            }
        },
    };

    let health_ok = match client
        .get(format!("{base_url}/api/v0.1/health"))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            record("OK", "daemon", format!("reachable at {base_url}"));
            true
        }
        Ok(resp) => {
            record(
                "FAIL",
                "daemon",
                format!("health returned non-success status {}", resp.status()),
            );
            false
        }
        Err(err) => {
            record("FAIL", "daemon", format!("unreachable: {err}"));
            false
        }
    };

    if health_ok {
        let auth = token.as_ref().map(|t| format!("Bearer {t}"));
        if let Some(auth) = auth {
            match client
                .get(format!("{base_url}/api/v0.1/info"))
                .header("Authorization", auth.clone())
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let info = resp.json::<InfoApiResponse>().await.ok();
                    let (github_cfg, gitlab_cfg) = info
                        .as_ref()
                        .and_then(|i| i.providers.as_ref())
                        .map(|p| (p.github_configured, p.gitlab_configured))
                        .unwrap_or((false, false));
                    let (bootstrap_cfg, bootstrap_cmd) = info
                        .as_ref()
                        .and_then(|i| i.commands.as_ref())
                        .map(|commands| {
                            let configured = commands.bootstrap_configured.unwrap_or(false);
                            let rendered = commands.bootstrap_program.as_ref().map(|program| {
                                let mut parts = vec![program.clone()];
                                parts.extend(commands.bootstrap_args.clone().unwrap_or_default());
                                parts.join(" ")
                            });
                            (configured, rendered)
                        })
                        .unwrap_or((false, None));
                    record(
                        if github_cfg { "OK" } else { "WARN" },
                        "provider.github",
                        if github_cfg {
                            "SHIREI_GITHUB_TOKEN configured in daemon".to_string()
                        } else {
                            "missing SHIREI_GITHUB_TOKEN in daemon env".to_string()
                        },
                    );
                    record(
                        if gitlab_cfg { "OK" } else { "WARN" },
                        "provider.gitlab",
                        if gitlab_cfg {
                            "SHIREI_GITLAB_HOST + SHIREI_GITLAB_TOKEN configured".to_string()
                        } else {
                            "missing SHIREI_GITLAB_HOST/SHIREI_GITLAB_TOKEN".to_string()
                        },
                    );
                    record(
                        "OK",
                        "commands.bootstrap",
                        match (bootstrap_cfg, bootstrap_cmd) {
                            (true, Some(cmd)) => format!("configured in daemon: {cmd}"),
                            (true, None) => "configured in daemon".to_string(),
                            (false, _) => "disabled (commands.bootstrap not set)".to_string(),
                        },
                    );

                    match client
                        .get(format!("{base_url}/api/v0.1/projects"))
                        .header("Authorization", auth)
                        .send()
                        .await
                    {
                        Ok(resp) if resp.status().is_success() => {
                            let projects = resp
                                .json::<ProjectsApiResponse>()
                                .await
                                .map(|v| v.projects)
                                .unwrap_or_default();
                            if projects.is_empty() {
                                record(
                                    "WARN",
                                    "projects",
                                    "no discovered projects; run: shirei project-discover ."
                                        .to_string(),
                                );
                            } else {
                                let selected = if let Some(pid) = project_filter.as_deref() {
                                    projects
                                        .into_iter()
                                        .filter(|p| p.project_id == pid)
                                        .collect::<Vec<_>>()
                                } else {
                                    projects
                                };

                                if selected.is_empty() {
                                    record(
                                        "WARN",
                                        "projects",
                                        "project filter matched no project".to_string(),
                                    );
                                } else {
                                    record(
                                        "OK",
                                        "projects",
                                        format!("{} project(s) checked", selected.len()),
                                    );
                                    let needs_github =
                                        selected.iter().any(|p| is_github_remote(&p.git_remote));
                                    let needs_gitlab =
                                        selected.iter().any(|p| is_gitlab_remote(&p.git_remote));

                                    if needs_github && !github_cfg {
                                        record(
                                            "FAIL",
                                            "auto_pr.github",
                                            "project remote requires GitHub; daemon missing SHIREI_GITHUB_TOKEN"
                                                .to_string(),
                                        );
                                    } else if needs_github {
                                        record(
                                            "OK",
                                            "auto_pr.github",
                                            "GitHub remote + token ready for auto PR/watch"
                                                .to_string(),
                                        );
                                    }

                                    if needs_gitlab && !gitlab_cfg {
                                        record(
                                            "FAIL",
                                            "auto_pr.gitlab",
                                            "project remote requires GitLab; daemon missing SHIREI_GITLAB_HOST/TOKEN"
                                                .to_string(),
                                        );
                                    } else if needs_gitlab {
                                        record(
                                            "OK",
                                            "auto_pr.gitlab",
                                            "GitLab remote + token ready for auto PR/watch"
                                                .to_string(),
                                        );
                                    }

                                    for p in selected {
                                        if p.default_branch.trim().is_empty() {
                                            record(
                                                "WARN",
                                                &format!("project.{}", p.project_id),
                                                "default_branch is empty".to_string(),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Ok(resp) => record(
                            "WARN",
                            "projects",
                            format!("failed to list projects: {}", resp.status()),
                        ),
                        Err(err) => record(
                            "WARN",
                            "projects",
                            format!("failed to list projects: {err}"),
                        ),
                    }
                }
                Ok(resp) => record(
                    "WARN",
                    "auth",
                    format!("/info denied with status {}; check token", resp.status()),
                ),
                Err(err) => record("WARN", "auth", format!("failed querying /info: {err}")),
            }
        } else {
            record(
                "WARN",
                "auth",
                "token missing; cannot inspect provider/project auth readiness".to_string(),
            );
        }
    }

    let summary = format!("doctor summary: {} fail, {} warn", failures, warnings);
    if failures > 0 {
        println!("[FAIL] {summary}");
        return Err(anyhow!(summary));
    }
    if warnings > 0 {
        println!("[WARN] {summary}");
    } else {
        println!("[OK] doctor summary: all checks passed");
    }
    Ok(())
}

fn is_github_remote(remote: &str) -> bool {
    let lower = remote.to_ascii_lowercase();
    lower.contains("github.com") || lower.starts_with("github:")
}

fn is_gitlab_remote(remote: &str) -> bool {
    let lower = remote.to_ascii_lowercase();
    lower.contains("gitlab") || lower.starts_with("gitlab:")
}

fn state_dir(state_dir: Option<&str>) -> anyhow::Result<PathBuf> {
    if let Some(dir) = state_dir {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(dir) = std::env::var("SHIREI_STATE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".local/share/shirei"))
}

fn token_candidates(state_dir: Option<&str>) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    if let Some(dir) = state_dir {
        paths.push(PathBuf::from(dir).join("token"));
    } else if let Ok(dir) = std::env::var("SHIREI_STATE_DIR") {
        paths.push(PathBuf::from(dir).join("token"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME not set"))?;
    paths.push(PathBuf::from(home).join(".local/share/shirei/token"));
    Ok(paths)
}

fn read_default_token(state_dir: Option<&str>) -> anyhow::Result<String> {
    let candidates = token_candidates(state_dir)?;
    for token_path in &candidates {
        if !token_path.exists() {
            continue;
        }
        let token = std::fs::read_to_string(token_path)
            .with_context(|| format!("failed reading {}", token_path.display()))?;
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    let tried = candidates
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(anyhow!(
        "missing token; pass --token or create token file (tried: {tried})"
    ))
}

fn authorization_header(token: Option<&str>, state_dir: Option<&str>) -> anyhow::Result<String> {
    let token = match token {
        Some(token) if !token.trim().is_empty() => token.to_string(),
        _ => read_default_token(state_dir)?,
    };
    Ok(format!("Bearer {token}"))
}

fn daemon_pid_path(state_dir: &Path) -> PathBuf {
    state_dir.join("daemon.pid")
}

fn read_daemon_pid(pid_path: &Path) -> anyhow::Result<Option<u32>> {
    if !pid_path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(pid_path)
        .with_context(|| format!("failed reading {}", pid_path.display()))?;
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let pid = raw.parse::<u32>().with_context(|| {
        format!(
            "invalid pid file {}; expected integer, got {raw}",
            pid_path.display()
        )
    })?;
    Ok(Some(pid))
}

fn write_daemon_pid(pid_path: &Path, pid: u32) -> anyhow::Result<()> {
    std::fs::write(pid_path, format!("{pid}\n"))
        .with_context(|| format!("failed writing {}", pid_path.display()))
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as i32, 0) };
    if rc == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error().raw_os_error();
    !matches!(err, Some(libc::ESRCH))
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn send_signal(pid: u32, sig: i32) -> anyhow::Result<()> {
    let rc = unsafe { libc::kill(pid as i32, sig) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(anyhow!("failed sending signal {sig} to pid {pid}: {err}"))
}

#[cfg(not(unix))]
fn send_signal(_pid: u32, _sig: i32) -> anyhow::Result<()> {
    Err(anyhow!(
        "stop/restart lifecycle commands are only supported on unix"
    ))
}

fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !process_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !process_alive(pid)
}

fn resolve_bind_from_base_url(base_url: &str) -> anyhow::Result<String> {
    let parsed = reqwest::Url::parse(base_url)
        .with_context(|| format!("invalid --base-url value: {base_url}"))?;
    if parsed.scheme() != "http" {
        return Err(anyhow!(
            "--base-url must use http for local daemon lifecycle commands"
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("--base-url is missing a host"))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| anyhow!("--base-url is missing a port"))?;
    Ok(format!("{host}:{port}"))
}

fn resolve_daemon_binary() -> String {
    std::env::var("SHIREI_DAEMON_BIN").unwrap_or_else(|_| "shirei-daemon".to_string())
}

async fn daemon_healthy(client: &reqwest::Client, base_url: &str) -> bool {
    match client
        .get(format!("{base_url}/api/v0.1/health"))
        .send()
        .await
    {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn wait_for_health(client: &reqwest::Client, base_url: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if daemon_healthy(client, base_url).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    daemon_healthy(client, base_url).await
}

async fn start_daemon(
    client: &reqwest::Client,
    base_url: &str,
    state_dir_arg: Option<&str>,
) -> anyhow::Result<()> {
    let state_dir = state_dir(state_dir_arg)?;
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed creating {}", state_dir.display()))?;
    let pid_path = daemon_pid_path(&state_dir);

    if let Some(pid) = read_daemon_pid(&pid_path)? {
        if process_alive(pid) {
            println!("Shirei daemon already running (pid {pid})");
            return Ok(());
        }
        let _ = std::fs::remove_file(&pid_path);
    }
    if daemon_healthy(client, base_url).await {
        println!("Shirei daemon already running at {base_url}");
        return Ok(());
    }

    let bind = resolve_bind_from_base_url(base_url)?;
    let daemon_bin = resolve_daemon_binary();
    let mut cmd = std::process::Command::new(&daemon_bin);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("SHIREI_BIND", bind);
    if let Some(dir) = state_dir_arg {
        cmd.env("SHIREI_STATE_DIR", dir);
    }

    let child = cmd
        .spawn()
        .with_context(|| {
            format!("failed to start daemon binary {daemon_bin}; install shirei-daemon or set SHIREI_DAEMON_BIN")
        })?;
    let pid = child.id();
    write_daemon_pid(&pid_path, pid)?;

    if wait_for_health(client, base_url, Duration::from_secs(8)).await {
        println!("Started Shirei daemon (pid {pid}) at {base_url}");
        return Ok(());
    }

    let _ = send_signal(pid, libc::SIGTERM);
    let _ = std::fs::remove_file(&pid_path);
    Err(anyhow!(
        "daemon did not become healthy after startup; check logs for shirei-daemon"
    ))
}

async fn stop_daemon(base_url: &str, state_dir_arg: Option<&str>) -> anyhow::Result<()> {
    let state_dir = state_dir(state_dir_arg)?;
    let pid_path = daemon_pid_path(&state_dir);
    let Some(pid) = read_daemon_pid(&pid_path)? else {
        if reqwest::get(format!("{base_url}/api/v0.1/health"))
            .await
            .map(|resp| resp.status().is_success())
            .unwrap_or(false)
        {
            println!(
                "Daemon is running at {base_url}, but no pid file found at {}; stop it manually",
                pid_path.display()
            );
            return Ok(());
        }
        println!("Shirei daemon already stopped");
        return Ok(());
    };

    if !process_alive(pid) {
        let _ = std::fs::remove_file(&pid_path);
        println!("Shirei daemon already stopped (removed stale pid file)");
        return Ok(());
    }

    send_signal(pid, libc::SIGTERM)?;
    if !wait_for_process_exit(pid, Duration::from_secs(5)) {
        send_signal(pid, libc::SIGKILL)?;
        let _ = wait_for_process_exit(pid, Duration::from_secs(2));
    }
    let _ = std::fs::remove_file(&pid_path);
    println!("Stopped Shirei daemon (pid {pid})");
    Ok(())
}

fn setup_opencode_commands(force: bool, print_only: bool) -> anyhow::Result<()> {
    let commands_dir = opencode_commands_dir()?;
    std::fs::create_dir_all(&commands_dir)
        .with_context(|| format!("failed creating {}", commands_dir.display()))?;

    let files = [
        (
            "shirei-attach.md",
            r#"---
description: Attach shirei session in this terminal
---
Run this command in the shell and replace `<job-id>` with the session id:

`shirei jobs <job-id> attach`

Use this when you want to open OpenCode directly inside a Shirei job worktree.
If the `shirei-workflow` skill is available, load it first.
"#,
        ),
        (
            "shirei-edit.md",
            r#"---
description: Open editor on a shirei job worktree
---
Run this command in the shell and replace `<job-id>` with the session id:

`shirei jobs <job-id> edit`

This opens your configured editor at the selected Shirei job worktree.
If the `shirei-workflow` skill is available, load it first.
"#,
        ),
    ];

    for (name, content) in files {
        let path = commands_dir.join(name);
        write_setup_file(&path, content, force, print_only)?;
    }

    let skills_dir = opencode_skills_dir()?;
    std::fs::create_dir_all(&skills_dir)
        .with_context(|| format!("failed creating {}", skills_dir.display()))?;
    let skill_path = skills_dir.join("shirei-workflow").join("SKILL.md");
    let skill_parent = skill_path
        .parent()
        .ok_or_else(|| anyhow!("invalid skill path {}", skill_path.display()))?;
    std::fs::create_dir_all(skill_parent)
        .with_context(|| format!("failed creating {}", skill_parent.display()))?;
    write_setup_file(
        &skill_path,
        r#"---
name: shirei-workflow
description: Operate Shirei sessions correctly from OpenCode
license: MIT
compatibility: opencode
---
## What this skill does
- Use Shirei for session lifecycle, job discovery, logs, and review actions.
- Keep work in the selected Shirei job worktree when attaching/editing sessions.

## Core commands
- Health/discovery:
  - `TOKEN=$(cat ~/.local/share/shirei/token)`
  - `shirei --token "$TOKEN" health`
  - `shirei --token "$TOKEN" project-discover .`
- Job operations:
  - `shirei --token "$TOKEN" jobs <project-id>`
  - `shirei --token "$TOKEN" jobs <job-id> attach`
  - Detach from attached OpenCode with `/q` (backend remains running)
  - `shirei --token "$TOKEN" jobs <job-id> attach-status`
  - `shirei --token "$TOKEN" jobs <job-id> attach-stop`
  - `shirei --token "$TOKEN" jobs <job-id> edit`
  - `shirei --token "$TOKEN" logs <job-id> --tail-lines 200`
- Review loop:
  - Use `shirei` to open the active-session picker, then attach in tmux
  - In PR/MR comments: `/shirei approve` or `/shirei request-changes <feedback>`

## When to use
- Use this skill whenever a task involves Shirei sessions, reruns, attach/edit, or PR review decisions.
"#,
        force,
        print_only,
    )?;

    let agents_dir = opencode_agents_dir()?;
    std::fs::create_dir_all(&agents_dir)
        .with_context(|| format!("failed creating {}", agents_dir.display()))?;
    let agent_path = agents_dir.join("shirei.md");
    write_setup_file(
        &agent_path,
        r#"---
description: Use for Shirei-managed coding and PR feedback sessions
mode: primary
---
You are the Shirei agent. Use this agent for sessions launched by Shirei jobs.

Core behavior:
- Follow repository instructions and local conventions first.
- Keep changes minimal, production-safe, and verifiable.
- When addressing PR feedback, apply fixes, run relevant tests, commit, and push before reporting completion.
- Prefer loading the `shirei-workflow` skill for Shirei lifecycle operations.

Output expectations:
- Be concise and actionable.
- Include changed file paths in final summaries.
- Include test/validation commands run and their result.
"#,
        force,
        print_only,
    )?;

    if !print_only {
        println!(
            "OpenCode commands installed in {}: /shirei-attach, /shirei-edit",
            commands_dir.display()
        );
        println!("OpenCode skill installed: {}", skill_path.display());
        println!("OpenCode agent installed: {}", agent_path.display());
    }
    Ok(())
}

fn write_setup_file(
    path: &Path,
    content: &str,
    force: bool,
    print_only: bool,
) -> anyhow::Result<()> {
    if print_only {
        println!("--- {} ---", path.display());
        println!("{}", content.trim_end());
        return Ok(());
    }

    if path.exists() && !force {
        println!(
            "skip existing {} (use --force to overwrite)",
            path.display()
        );
        return Ok(());
    }

    std::fs::write(path, content).with_context(|| format!("failed writing {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

fn opencode_commands_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("OPENCODE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir).join("commands"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".config/opencode/commands"))
}

fn opencode_skills_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("OPENCODE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir).join("skills"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".config/opencode/skills"))
}

fn opencode_agents_dir() -> anyhow::Result<PathBuf> {
    if let Ok(dir) = std::env::var("OPENCODE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir).join("agent"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".config/opencode/agent"))
}

async fn attach_job(
    client: &reqwest::Client,
    base_url: &str,
    auth: &str,
    job_id: &str,
) -> anyhow::Result<()> {
    let v: AttachApiResponse = send_api_request(base_url, || {
        client
            .post(format!("{}/api/v0.1/jobs/{}/attach", base_url, job_id))
            .header("Authorization", auth)
    })
    .await?
    .error_for_status()?
    .json()
    .await?;

    if !Path::new(&v.attach.cwd).exists() {
        return Err(anyhow!(
            "job {} attach directory is unavailable at {}",
            job_id,
            v.attach.cwd
        ));
    }

    eprintln!(
        "Attaching opencode to job {} in {} (Ctrl+C to detach)",
        job_id, v.attach.cwd
    );

    let status = std::process::Command::new(&v.attach.cmd)
        .args(&v.attach.argv)
        .current_dir(&v.attach.cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to launch attach command {}", v.attach.cmd))?;

    if !status.success() {
        return Err(anyhow!(
            "attach command exited with status {}",
            status.code().unwrap_or(-1)
        ));
    }

    Ok(())
}

async fn edit_job(
    client: &reqwest::Client,
    base_url: &str,
    auth: &str,
    job_id: &str,
) -> anyhow::Result<()> {
    let v: JobApiResponse = send_api_request(base_url, || {
        client
            .get(format!("{}/api/v0.1/jobs/{}", base_url, job_id))
            .header("Authorization", auth)
    })
    .await?
    .error_for_status()?
    .json()
    .await?;

    let target_path = resolve_job_target_path(&v.job)?;
    let editor_cmd = resolve_editor_command(&load_config()?)?;
    let status = spawn_editor_command(&editor_cmd, &target_path)?;
    if !status.success() {
        return Err(anyhow!(
            "editor command failed with status {}",
            status.code().unwrap_or(-1)
        ));
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "opened": true,
            "job_id": job_id,
            "path": target_path,
        }))?
    );

    Ok(())
}

async fn attach_status_job(
    client: &reqwest::Client,
    base_url: &str,
    auth: &str,
    job_id: &str,
) -> anyhow::Result<()> {
    let v: serde_json::Value = send_api_request(base_url, || {
        client
            .get(format!(
                "{}/api/v0.1/jobs/{}/attach-status",
                base_url, job_id
            ))
            .header("Authorization", auth)
    })
    .await?
    .error_for_status()?
    .json()
    .await?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

async fn attach_stop_job(
    client: &reqwest::Client,
    base_url: &str,
    auth: &str,
    job_id: &str,
) -> anyhow::Result<()> {
    let v: serde_json::Value = send_api_request(base_url, || {
        client
            .post(format!("{}/api/v0.1/jobs/{}/attach-stop", base_url, job_id))
            .header("Authorization", auth)
    })
    .await?
    .error_for_status()?
    .json()
    .await?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

fn resolve_job_target_path(job: &JobRecord) -> anyhow::Result<PathBuf> {
    if let Some(wt_path) = job.worktree_path.as_deref() {
        let wt_path = PathBuf::from(wt_path);
        if wt_path.exists() {
            return Ok(wt_path);
        }
        return Err(anyhow!(
            "job {} worktree is unavailable at {}",
            job.job_id,
            wt_path.display()
        ));
    }

    let cwd_path = PathBuf::from(&job.cwd);
    if cwd_path.exists() {
        return Ok(cwd_path);
    }

    Err(anyhow!(
        "job {} has no accessible worktree/cwd (missing {})",
        job.job_id,
        cwd_path.display()
    ))
}

fn resolve_editor_command(config: &CliConfig) -> anyhow::Result<String> {
    let configured = config.editor_command().map(str::to_string);
    let visual = std::env::var("VISUAL")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let editor = std::env::var("EDITOR")
        .ok()
        .filter(|v| !v.trim().is_empty());

    configured
        .or(visual)
        .or(editor)
        .or_else(|| Some("code".to_string()))
        .ok_or_else(|| anyhow!("no editor command configured"))
}

fn config_path() -> anyhow::Result<PathBuf> {
    if let Ok(path) = std::env::var("SHIREI_CONFIG") {
        return Ok(PathBuf::from(path));
    }

    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("shirei")
        .join("config.toml"))
}

fn load_config() -> anyhow::Result<CliConfig> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(CliConfig::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed reading config at {}", path.display()))?;
    let cfg = toml::from_str::<CliConfig>(&raw)
        .with_context(|| format!("failed parsing config at {}", path.display()))?;
    Ok(cfg)
}

fn spawn_editor_command(
    editor_cmd: &str,
    target_path: &Path,
) -> anyhow::Result<std::process::ExitStatus> {
    let (program, args) = build_editor_invocation(editor_cmd, target_path)?;
    std::process::Command::new(program)
        .args(args)
        .status()
        .context("failed to launch configured editor command")
}

fn build_editor_invocation(
    editor_cmd: &str,
    target_path: &Path,
) -> anyhow::Result<(String, Vec<String>)> {
    let mut parts = shell_words::split(editor_cmd)
        .with_context(|| format!("invalid editor command: {editor_cmd}"))?;
    if parts.is_empty() {
        return Err(anyhow!("editor command is empty"));
    }

    let target = target_path.display().to_string();
    let mut has_placeholder = false;
    for part in parts.iter_mut().skip(1) {
        if part.contains("{path}") {
            has_placeholder = true;
            *part = part.replace("{path}", &target);
        }
    }
    if parts[0].contains("{path}") {
        has_placeholder = true;
        parts[0] = parts[0].replace("{path}", &target);
    }

    let program = parts.remove(0);
    if !has_placeholder {
        parts.push(target);
    }

    Ok((program, parts))
}

#[cfg(test)]
mod tests {
    use super::{
        build_editor_invocation, resolve_bind_from_base_url, resolve_editor_command,
        resolve_job_target_path, send_api_request_with, tmux_session_name, token_candidates,
        wait_for_daemon_ready, Args, CliConfig, Cmd, EditorConfig, JobRecord, JobsCmd, SetupCmd,
    };
    use clap::Parser;
    use std::net::SocketAddr;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn reserve_local_addr() -> SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral addr");
        let addr = listener.local_addr().expect("resolve local addr");
        drop(listener);
        addr
    }

    async fn run_test_server(addr: SocketAddr) {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("bind test server");
        loop {
            let (mut stream, _) = listener.accept().await.expect("accept test request");
            let mut buf = [0_u8; 2048];
            let n = stream.read(&mut buf).await.expect("read request");
            let req = String::from_utf8_lossy(&buf[..n]);

            let (status, body) = if req.starts_with("GET /api/v0.1/health ") {
                ("200 OK", r#"{"ok":true}"#)
            } else if req.starts_with("GET /api/v0.1/info ") {
                ("200 OK", r#"{"name":"shirei-test"}"#)
            } else {
                ("404 Not Found", r#"{"error":"missing"}"#)
            };

            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
    }

    #[test]
    fn appends_path_when_placeholder_absent() {
        let target = std::path::PathBuf::from("/tmp/worktree");
        let (program, args) =
            build_editor_invocation("code --reuse-window", &target).expect("valid command");
        assert_eq!(program, "code");
        assert_eq!(args, vec!["--reuse-window", "/tmp/worktree"]);
    }

    #[test]
    fn parses_jobs_attach_subcommand() {
        let args = Args::parse_from(["shirei", "jobs", "job_123", "attach"]);
        match args.cmd {
            Some(Cmd::Jobs { id, action }) => {
                assert_eq!(id, "job_123");
                assert!(matches!(action, Some(JobsCmd::Attach)));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_lifecycle_commands() {
        assert!(matches!(
            Args::parse_from(["shirei", "start"]).cmd,
            Some(Cmd::Start)
        ));
        assert!(matches!(
            Args::parse_from(["shirei", "stop"]).cmd,
            Some(Cmd::Stop)
        ));
        assert!(matches!(
            Args::parse_from(["shirei", "restart"]).cmd,
            Some(Cmd::Restart)
        ));
        assert!(matches!(
            Args::parse_from(["shirei", "doctor"]).cmd,
            Some(Cmd::Doctor { .. })
        ));
    }

    #[test]
    fn allows_running_without_subcommand() {
        let args = Args::parse_from(["shirei"]);
        assert!(args.cmd.is_none());
    }

    #[test]
    fn parses_branch_command() {
        let args = Args::parse_from(["shirei", "branch", "feature/demo", "--prompt", "hello"]);
        match args.cmd {
            Some(Cmd::Branch {
                branch_name,
                prompt,
            }) => {
                assert_eq!(branch_name.as_deref(), Some("feature/demo"));
                assert_eq!(prompt.as_deref(), Some("hello"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn branch_command_supports_picker_mode() {
        let args = Args::parse_from(["shirei", "branch"]);
        match args.cmd {
            Some(Cmd::Branch {
                branch_name,
                prompt,
            }) => {
                assert!(branch_name.is_none());
                assert!(prompt.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_setup_opencode_command() {
        let args = Args::parse_from(["shirei", "setup", "opencode", "--force"]);
        match args.cmd {
            Some(Cmd::Setup { target }) => match target {
                SetupCmd::Opencode { force, print } => {
                    assert!(force);
                    assert!(!print);
                }
            },
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn replaces_placeholder_when_present() {
        let target = std::path::PathBuf::from("/tmp/worktree");
        let (program, args) = build_editor_invocation("code --folder-uri file://{path}", &target)
            .expect("valid command");
        assert_eq!(program, "code");
        assert_eq!(args, vec!["--folder-uri", "file:///tmp/worktree"]);
    }

    #[test]
    fn prefers_editor_from_config() {
        let cfg = CliConfig {
            editor_command: None,
            editor: Some(EditorConfig {
                command: Some("nvim".to_string()),
            }),
        };
        let cmd = resolve_editor_command(&cfg).expect("editor command");
        assert_eq!(cmd, "nvim");
    }

    #[test]
    fn missing_worktree_path_returns_friendly_error() {
        let job = JobRecord {
            job_id: "job_123".to_string(),
            name: "demo".to_string(),
            cwd: "/definitely/missing".to_string(),
            worktree_path: Some("/definitely/missing".to_string()),
        };

        let err = resolve_job_target_path(&job).expect_err("should fail");
        assert!(
            err.to_string().contains("worktree is unavailable"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn bootstraps_daemon_and_retries_once_when_unreachable() {
        let addr = reserve_local_addr();
        let base_url = format!("http://{addr}");
        let client = reqwest::Client::new();
        let bootstrap_calls = Arc::new(AtomicUsize::new(0));
        let server_handle = Arc::new(Mutex::new(None));

        let response = send_api_request_with(
            &base_url,
            || client.get(format!("{}/api/v0.1/info", base_url)),
            {
                let bootstrap_calls = Arc::clone(&bootstrap_calls);
                let server_handle = Arc::clone(&server_handle);
                move || {
                    let bootstrap_calls = Arc::clone(&bootstrap_calls);
                    let server_handle = Arc::clone(&server_handle);
                    async move {
                        bootstrap_calls.fetch_add(1, Ordering::SeqCst);
                        let mut guard = server_handle.lock().expect("lock handle slot");
                        if guard.is_none() {
                            *guard = Some(tokio::spawn(run_test_server(addr)));
                        }
                        Ok(())
                    }
                }
            },
        )
        .await
        .expect("request succeeds after bootstrap");

        assert!(response.status().is_success());
        assert_eq!(bootstrap_calls.load(Ordering::SeqCst), 1);

        let handle = {
            let mut guard = server_handle.lock().expect("lock handle slot");
            guard.take()
        };
        if let Some(handle) = handle {
            handle.abort();
        }
    }

    #[tokio::test]
    async fn does_not_bootstrap_when_daemon_is_already_healthy() {
        let addr = reserve_local_addr();
        let base_url = format!("http://{addr}");
        let client = reqwest::Client::new();
        let bootstrap_calls = Arc::new(AtomicUsize::new(0));
        let server_handle = tokio::spawn(run_test_server(addr));
        wait_for_daemon_ready(&base_url)
            .await
            .expect("test server becomes healthy");

        let response = send_api_request_with(
            &base_url,
            || client.get(format!("{}/api/v0.1/info", base_url)),
            {
                let bootstrap_calls = Arc::clone(&bootstrap_calls);
                move || {
                    let bootstrap_calls = Arc::clone(&bootstrap_calls);
                    async move {
                        bootstrap_calls.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            },
        )
        .await
        .expect("request succeeds without bootstrap");

        assert!(response.status().is_success());
        assert_eq!(bootstrap_calls.load(Ordering::SeqCst), 0);

        server_handle.abort();
    }

    #[test]
    fn token_candidates_include_state_dir_first() {
        let candidates = token_candidates(Some("/tmp/shirei-state")).expect("token paths");
        assert_eq!(candidates[0].to_string_lossy(), "/tmp/shirei-state/token");
    }

    #[test]
    fn resolve_bind_from_base_url_supports_explicit_port() {
        let bind = resolve_bind_from_base_url("http://127.0.0.1:9393").expect("bind");
        assert_eq!(bind, "127.0.0.1:9393");
    }

    #[test]
    fn tmux_session_name_is_stable_and_short() {
        let name = tmux_session_name("feature/add picker", "job_1234567890");
        assert_eq!(name, "shirei-feature-add-picker-12345678");
    }
}
