use anyhow::{anyhow, Context};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;

#[derive(Parser, Debug)]
#[command(name = "shirei", version, about = "Shirei CLI")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:7373")]
    base_url: String,
    #[arg(long)]
    token: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
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
    let auth = format!("Bearer {}", a.token);

    match a.cmd {
        Cmd::Health => {
            let v: serde_json::Value = c
                .get(format!("{}/api/v0.1/health", a.base_url))
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Info => {
            let v: serde_json::Value = c
                .get(format!("{}/api/v0.1/info", a.base_url))
                .header("Authorization", auth)
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::ProjectDiscover { path } => {
            let v: serde_json::Value = c
                .post(format!("{}/api/v0.1/projects/discover", a.base_url))
                .header("Authorization", auth)
                .json(&serde_json::json!({"root_path": path}))
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Projects => {
            let v: serde_json::Value = c
                .get(format!("{}/api/v0.1/projects", a.base_url))
                .header("Authorization", auth)
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Jobs { id, action } => match action {
            Some(JobsCmd::Attach) => attach_job(&c, &a.base_url, &auth, &id).await?,
            Some(JobsCmd::Edit) => edit_job(&c, &a.base_url, &auth, &id).await?,
            None => {
                let v: serde_json::Value = c
                    .get(format!("{}/api/v0.1/projects/{}/jobs", a.base_url, id))
                    .header("Authorization", auth)
                    .send()
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
            let v: serde_json::Value = c
                .post(format!(
                    "{}/api/v0.1/projects/{}/jobs",
                    a.base_url, project_id
                ))
                .header("Authorization", auth)
                .json(&serde_json::json!({"name": name, "prompt": prompt}))
                .send()
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
            let v: serde_json::Value = c
                .post(format!("{}/api/v0.1/watch-pr", a.base_url))
                .header("Authorization", auth)
                .json(&serde_json::json!({"project_id": project_id, "repo_key": repo_key, "pr_number": pr_number, "retention_days": retention_days}))
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::WatchList => {
            let v: serde_json::Value = c
                .get(format!("{}/api/v0.1/watch-pr", a.base_url))
                .header("Authorization", auth)
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::WatchStop { watch_id } => {
            let v: serde_json::Value = c
                .post(format!(
                    "{}/api/v0.1/watch-pr/{}/stop",
                    a.base_url, watch_id
                ))
                .header("Authorization", auth)
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Logs { job_id, tail_lines } => {
            let url = if let Some(n) = tail_lines {
                format!(
                    "{}/api/v0.1/jobs/{}/logs?tail_lines={}",
                    a.base_url, job_id, n
                )
            } else {
                format!("{}/api/v0.1/jobs/{}/logs", a.base_url, job_id)
            };
            let v: serde_json::Value = c
                .get(url)
                .header("Authorization", auth)
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
    }

    Ok(())
}

async fn attach_job(
    client: &reqwest::Client,
    base_url: &str,
    auth: &str,
    job_id: &str,
) -> anyhow::Result<()> {
    let v: AttachApiResponse = client
        .post(format!("{}/api/v0.1/jobs/{}/attach", base_url, job_id))
        .header("Authorization", auth)
        .send()
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
    let v: JobApiResponse = client
        .get(format!("{}/api/v0.1/jobs/{}", base_url, job_id))
        .header("Authorization", auth)
        .send()
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

#[cfg(test)]
mod tests {
    use super::{
        build_editor_invocation, resolve_editor_command, resolve_job_target_path, Args, CliConfig,
        Cmd, EditorConfig, JobRecord, JobsCmd,
    };
    use clap::Parser;

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
        let args = Args::parse_from(["shirei", "--token", "t", "jobs", "job_123", "attach"]);
        match args.cmd {
            Cmd::Jobs { id, action } => {
                assert_eq!(id, "job_123");
                assert!(matches!(action, Some(JobsCmd::Attach)));
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
}
