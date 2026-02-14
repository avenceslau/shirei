use anyhow::{anyhow, Context};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::future::Future;
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
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    Start,
    Stop,
    Restart,
    Tui {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Health,
    Info,
    ProjectDiscover {
        path: String,
    },
    Projects,
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
}

#[derive(Debug, Deserialize)]
struct JobApiResponse {
    job: JobRecord,
}

#[derive(Debug, Deserialize)]
struct JobRecord {
    job_id: String,
    cwd: String,
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

    match a.cmd {
        Cmd::Start => start_daemon(&c, &a.base_url, a.state_dir.as_deref()).await?,
        Cmd::Stop => stop_daemon(&a.base_url, a.state_dir.as_deref()).await?,
        Cmd::Restart => {
            stop_daemon(&a.base_url, a.state_dir.as_deref()).await?;
            start_daemon(&c, &a.base_url, a.state_dir.as_deref()).await?;
        }
        Cmd::Tui { args } => {
            launch_tui(
                &a.base_url,
                a.token.as_deref(),
                a.state_dir.as_deref(),
                &args,
            )?;
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
        Cmd::Jobs { id, action } => match action {
            Some(JobsCmd::Attach) => {
                let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
                attach_job(&c, &a.base_url, &auth, &id).await?
            }
            Some(JobsCmd::Edit) => {
                let auth = authorization_header(a.token.as_deref(), a.state_dir.as_deref())?;
                edit_job(&c, &a.base_url, &auth, &id).await?
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
        .context("failed to spawn shirei-daemon")?;
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

fn resolve_tui_binary() -> String {
    std::env::var("SHIREI_TUI_BIN").unwrap_or_else(|_| "shirei-tui".to_string())
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
        .with_context(|| format!("failed to start daemon binary {daemon_bin}"))?;
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

fn launch_tui(
    base_url: &str,
    token: Option<&str>,
    state_dir: Option<&str>,
    pass_through_args: &[String],
) -> anyhow::Result<()> {
    let mut cmd = std::process::Command::new(resolve_tui_binary());
    cmd.arg("--base-url").arg(base_url);

    if let Some(token) = token {
        cmd.arg("--token").arg(token);
    } else if let Ok(token) = read_default_token(state_dir) {
        cmd.arg("--token").arg(token);
    }

    if let Some(dir) = state_dir {
        cmd.arg("--state-dir").arg(dir);
    }

    cmd.args(pass_through_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let status = cmd
        .status()
        .context("failed to launch shirei-tui binary; install or set SHIREI_TUI_BIN")?;
    if !status.success() {
        return Err(anyhow!(
            "shirei-tui exited with status {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
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
        .context("failed to spawn shirei-daemon")?;
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

#[cfg(test)]
mod tests {
    use super::{
        build_editor_invocation, resolve_bind_from_base_url, resolve_editor_command,
        resolve_job_target_path, send_api_request_with, token_candidates, wait_for_daemon_ready,
        Args, CliConfig, Cmd, EditorConfig, JobRecord, JobsCmd,
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
            Cmd::Jobs { id, action } => {
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
            Cmd::Start
        ));
        assert!(matches!(
            Args::parse_from(["shirei", "stop"]).cmd,
            Cmd::Stop
        ));
        assert!(matches!(
            Args::parse_from(["shirei", "restart"]).cmd,
            Cmd::Restart
        ));
    }

    #[test]
    fn parses_tui_passthrough_args() {
        let args = Args::parse_from(["shirei", "tui", "--", "--discover-path", "/tmp/repo"]);
        match args.cmd {
            Cmd::Tui { args } => {
                assert_eq!(args, vec!["--discover-path", "/tmp/repo"]);
            }
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
}
