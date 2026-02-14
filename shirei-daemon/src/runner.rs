use anyhow::Result;
use sqlx::SqlitePool;
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::Mutex,
};

use crate::{
    config::ShireiConfig,
    events::{EventBus, ShireiEvent},
    git_ops, store,
    util::now_rfc3339,
    vcs::{
        factory::ProviderFactory, parse_repo_key, repo_key_from_remote, CreatePrRequest,
        PullRequestId,
    },
};

const DEFAULT_CONTINUE_RETRIES: u32 = 6;
const CONTINUE_PROMPT: &str = "continue";

pub struct JobRunner {
    pool: SqlitePool,
    bus: EventBus,
    state_dir: PathBuf,
    providers: Arc<ProviderFactory>,
    config: Arc<ShireiConfig>,
}

impl JobRunner {
    pub fn new(
        pool: SqlitePool,
        bus: EventBus,
        state_dir: PathBuf,
        providers: Arc<ProviderFactory>,
        config: Arc<ShireiConfig>,
    ) -> Self {
        Self {
            pool,
            bus,
            state_dir,
            providers,
            config,
        }
    }

    pub async fn spawn_opencode_job(&self, job_id: &str) -> Result<()> {
        self.run_job_loop(job_id, false).await
    }

    pub async fn spawn_opencode_job_continue(&self, job_id: &str) -> Result<()> {
        self.run_job_loop(job_id, true).await
    }

    async fn run_job_loop(&self, job_id: &str, start_with_continue: bool) -> Result<()> {
        let logs_dir = self.state_dir.join("logs");
        fs::create_dir_all(&logs_dir).await?;
        let log_path = logs_dir.join(format!("{job_id}.log"));
        let log_path_str = log_path.display().to_string();

        let max_continue_retries = std::env::var("SHIREI_CONTINUE_RETRIES")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(DEFAULT_CONTINUE_RETRIES);

        let mut continue_attempts = 0_u32;
        let mut prompt_override = if start_with_continue {
            Some(CONTINUE_PROMPT.to_string())
        } else {
            None
        };

        loop {
            let job = store::get_job(&self.pool, job_id).await?;
            if job.status != "RUNNING" {
                tracing::info!(job_id = %job.job_id, status = %job.status, "job no longer running; skipping runner loop");
                return Ok(());
            }

            tracing::info!(
                job_id = %job.job_id,
                project_id = %job.project_id,
                cwd = %job.cwd,
                continue_attempts,
                "starting opencode job runtime"
            );

            let prompt = prompt_override.take().or_else(|| job.prompt.clone());
            let _ = store::set_job_attention(&self.pool, job_id, false).await;

            let (ok, exit_code) = self
                .spawn_once(&job, &log_path, &log_path_str, prompt.as_deref())
                .await?;

            if ok {
                self.ensure_pr_and_watch(&job).await;
                store::update_job_end(&self.pool, job_id, "AWAITING_REVIEW", exit_code).await?;
                tracing::info!(
                    job_id = %job.job_id,
                    status = "AWAITING_REVIEW",
                    exit_code = ?exit_code,
                    "job runtime completed"
                );
                let job_for_comment = store::get_job(&self.pool, job_id)
                    .await
                    .unwrap_or(job.clone());
                self.post_pr_completion_comment(&job_for_comment, exit_code, true)
                    .await;
                return Ok(());
            }

            let status = store::job_status(&self.pool, job_id).await?;
            if status.as_deref() != Some("RUNNING") {
                tracing::warn!(
                    job_id = %job.job_id,
                    status = ?status,
                    exit_code = ?exit_code,
                    "job process exited and job is no longer RUNNING; not restarting"
                );
                return Ok(());
            }

            if continue_attempts >= max_continue_retries {
                store::update_job_end(&self.pool, job_id, "ERROR", exit_code).await?;
                tracing::warn!(
                    job_id = %job.job_id,
                    exit_code = ?exit_code,
                    max_continue_retries,
                    "job process kept exiting; reached continue retry limit"
                );
                self.post_pr_completion_comment(&job, exit_code, false)
                    .await;
                return Ok(());
            }

            continue_attempts += 1;
            prompt_override = Some(CONTINUE_PROMPT.to_string());
            tracing::warn!(
                job_id = %job.job_id,
                exit_code = ?exit_code,
                continue_attempts,
                max_continue_retries,
                "job process exited while RUNNING; restarting with continue"
            );
            tokio::time::sleep(Duration::from_millis(600)).await;
        }
    }

    async fn spawn_once(
        &self,
        job: &store::JobRow,
        log_path: &std::path::Path,
        log_path_str: &str,
        prompt: Option<&str>,
    ) -> Result<(bool, Option<i64>)> {
        let mut log_out = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .await?;
        let mut log_err = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .await?;

        let (cmd_bin, cfg_args) = match std::env::var("SHIREI_OPENCODE_CMD") {
            Ok(cmd) => (cmd, Vec::new()),
            Err(_) => (
                self.config.commands.opencode.program.clone(),
                self.config.commands.opencode.args.clone(),
            ),
        };
        tracing::debug!(
            job_id = %job.job_id,
            cmd = %cmd_bin,
            args = ?cfg_args,
            prompt = ?prompt,
            "resolved job command"
        );

        let mut cmd = Command::new(&cmd_bin);
        cmd.args(&cfg_args);
        if let Some(p) = prompt {
            cmd.arg("run").arg(p);
        }
        cmd.current_dir(&job.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = cmd.spawn()?;
        let pid = child.id().map(|p| p as i64);
        tracing::info!(job_id = %job.job_id, pid = ?pid, log_path = %log_path_str, "job process spawned");

        store::update_job_runtime(&self.pool, &job.job_id, pid, Some(log_path_str)).await?;

        self.bus.publish(ShireiEvent {
            event_id: format!("evt_{}", uuid::Uuid::new_v4()),
            ts: now_rfc3339(),
            project_id: job.project_id.clone(),
            subject_id: job.job_id.clone(),
            r#type: "JOB_STARTED".to_string(),
            payload: serde_json::json!({"pid": pid, "cwd": job.cwd, "log_path": log_path_str}),
        });

        if let Some(p) = prompt {
            let _ = log_out
                .write_all(format!("[runner] opencode run \"{}\"\n", p).as_bytes())
                .await;
        }

        let mut stdout_lines = child.stdout.take().map(|s| BufReader::new(s).lines());
        let mut stderr_lines = child.stderr.take().map(|s| BufReader::new(s).lines());

        let attention_flag = std::sync::Arc::new(Mutex::new(false));

        let bus1 = self.bus.clone();
        let proj1 = job.project_id.clone();
        let jid1 = job.job_id.clone();
        let pool1 = self.pool.clone();
        let attention1 = attention_flag.clone();
        let out_task = tokio::spawn(async move {
            if let Some(ref mut lines) = stdout_lines {
                while let Ok(Some(line)) = lines.next_line().await {
                    let _ = log_out
                        .write_all(format!("[stdout] {line}\n").as_bytes())
                        .await;
                    maybe_mark_waiting_input(&pool1, &jid1, &line, &attention1).await;
                    bus1.publish(ShireiEvent {
                        event_id: format!("evt_{}", uuid::Uuid::new_v4()),
                        ts: now_rfc3339(),
                        project_id: proj1.clone(),
                        subject_id: jid1.clone(),
                        r#type: "JOB_OUTPUT".to_string(),
                        payload: serde_json::json!({"stream":"stdout","line":line}),
                    });
                }
            }
        });

        let bus2 = self.bus.clone();
        let proj2 = job.project_id.clone();
        let jid2 = job.job_id.clone();
        let pool2 = self.pool.clone();
        let attention2 = attention_flag.clone();
        let err_task = tokio::spawn(async move {
            if let Some(ref mut lines) = stderr_lines {
                while let Ok(Some(line)) = lines.next_line().await {
                    let _ = log_err
                        .write_all(format!("[stderr] {line}\n").as_bytes())
                        .await;
                    maybe_mark_waiting_input(&pool2, &jid2, &line, &attention2).await;
                    bus2.publish(ShireiEvent {
                        event_id: format!("evt_{}", uuid::Uuid::new_v4()),
                        ts: now_rfc3339(),
                        project_id: proj2.clone(),
                        subject_id: jid2.clone(),
                        r#type: "JOB_OUTPUT".to_string(),
                        payload: serde_json::json!({"stream":"stderr","line":line}),
                    });
                }
            }
        });

        let status = child.wait().await?;
        let exit_code = status.code().map(|c| c as i64);
        let ok = exit_code.unwrap_or(1) == 0;

        let _ = out_task.await;
        let _ = err_task.await;

        Ok((ok, exit_code))
    }

    async fn post_pr_completion_comment(
        &self,
        job: &store::JobRow,
        exit_code: Option<i64>,
        ok: bool,
    ) {
        if let (Some(repo_key), Some(pr_number)) =
            (job.origin_repo_key.clone(), job.origin_pr_number)
        {
            if let Some((kind, repo)) = parse_repo_key(&repo_key) {
                let provider = self.providers.get(kind);
                let pr = PullRequestId {
                    repo,
                    number: pr_number as u64,
                };
                let body = format!(
                    "Shirei finished job `{}`\n\nStatus: {}\nExit: {:?}",
                    job.job_id,
                    if ok { "✅ success" } else { "❌ error" },
                    exit_code
                );
                if let Err(e) = provider.create_comment(&pr, &body).await {
                    tracing::warn!(
                        job_id = %job.job_id,
                        repo_key = %repo_key,
                        pr_number,
                        error = ?e,
                        "failed to post completion comment"
                    );
                }
            }
        }
    }

    async fn ensure_pr_and_watch(&self, job: &store::JobRow) {
        if job.origin_repo_key.is_some() && job.origin_pr_number.is_some() {
            return;
        }

        let Some(worktree_name) = job.worktree_name.as_ref() else {
            return;
        };
        let branch = format!("shirei/{worktree_name}");
        let project = match store::get_project(&self.pool, &job.project_id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(job_id = %job.job_id, error = ?e, "failed loading project for auto PR");
                return;
            }
        };

        let Some(repo_key) = repo_key_from_remote(&project.git_remote) else {
            tracing::info!(job_id = %job.job_id, git_remote = %project.git_remote, "skipping auto PR; unsupported git remote");
            return;
        };

        let Some((kind, repo)) = parse_repo_key(&repo_key) else {
            return;
        };

        let repo_handle = match git_ops::open_repo(std::path::Path::new(&project.root_path)) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(job_id = %job.job_id, error = ?e, "failed opening repo for auto PR");
                return;
            }
        };

        let base_ref = format!("refs/heads/{}", project.default_branch);
        let has_changes = git_ops::branch_ahead_of_base(&repo_handle, &branch, &base_ref)
            .or_else(|_| {
                git_ops::branch_ahead_of_base(
                    &repo_handle,
                    &branch,
                    &format!("refs/remotes/origin/{}", project.default_branch),
                )
            })
            .unwrap_or(false);
        if !has_changes {
            tracing::info!(job_id = %job.job_id, branch = %branch, "skipping auto PR; no branch diff");
            return;
        }

        if let Err(e) = git_ops::push_branch_origin(&repo_handle, &branch) {
            tracing::warn!(job_id = %job.job_id, branch = %branch, error = ?e, "failed pushing branch for auto PR");
            return;
        }

        let provider = self.providers.get(kind);
        let base = project.default_branch.clone();
        let title = format!("Shirei review: {}", job.name);
        let body = format!(
            "Auto-created by Shirei for job `{}`.\n\nReview with `/shirei approve` or `/shirei request-changes <feedback>`.",
            job.job_id
        );
        let created = match provider
            .create_pr(
                &PullRequestId { repo, number: 0 },
                &CreatePrRequest {
                    title,
                    body,
                    head_branch: branch.clone(),
                    base_branch: base,
                    draft: true,
                },
            )
            .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(job_id = %job.job_id, error = ?e, "failed creating auto PR");
                return;
            }
        };

        if let Err(e) = store::set_job_origin_pr(
            &self.pool,
            &job.job_id,
            &repo_key,
            created.number as i64,
            Some("auto_pr"),
        )
        .await
        {
            tracing::warn!(job_id = %job.job_id, error = ?e, "failed linking auto PR to job");
            return;
        }

        if let Err(e) = store::ensure_watch(
            &self.pool,
            &job.project_id,
            &repo_key,
            created.number as i64,
            7,
        )
        .await
        {
            tracing::warn!(job_id = %job.job_id, error = ?e, "failed ensuring watch for auto PR");
            return;
        }

        tracing::info!(
            job_id = %job.job_id,
            repo_key = %repo_key,
            pr_number = created.number,
            pr_url = ?created.url,
            "auto draft PR created and watch attached"
        );
    }
}

pub async fn recover_running_jobs(
    pool: SqlitePool,
    bus: EventBus,
    state_dir: PathBuf,
    providers: Arc<ProviderFactory>,
    config: Arc<ShireiConfig>,
) -> Result<()> {
    let rows = store::list_running_jobs(&pool).await?;
    if rows.is_empty() {
        return Ok(());
    }

    tracing::info!(
        count = rows.len(),
        "recovering RUNNING jobs after daemon startup"
    );
    for (job_id, pid) in rows {
        if let Some(pid) = pid {
            if process_alive(pid) {
                tracing::info!(job_id = %job_id, pid, "job process still alive; keeping current runtime");
                continue;
            }
        }

        tracing::warn!(job_id = %job_id, pid = ?pid, "job marked RUNNING but process missing; restarting with continue");
        let runner = JobRunner::new(
            pool.clone(),
            bus.clone(),
            state_dir.clone(),
            providers.clone(),
            config.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = runner.spawn_opencode_job_continue(&job_id).await {
                tracing::error!(job_id = %job_id, error = ?e, "failed restarting stalled job");
            }
        });
    }
    Ok(())
}

#[cfg(unix)]
fn process_alive(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    let rc = unsafe { libc::kill(pid as i32, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: i64) -> bool {
    false
}

async fn maybe_mark_waiting_input(
    pool: &SqlitePool,
    job_id: &str,
    line: &str,
    attention_flag: &std::sync::Arc<Mutex<bool>>,
) {
    if !looks_like_waiting_input(line) {
        return;
    }

    let mut flag = attention_flag.lock().await;
    if *flag {
        return;
    }
    *flag = true;
    drop(flag);

    if let Err(e) = store::set_job_attention(pool, job_id, true).await {
        tracing::warn!(job_id = %job_id, error = ?e, "failed setting waiting-input attention");
    } else {
        tracing::info!(job_id = %job_id, "job requires user input/permission");
    }
}

fn looks_like_waiting_input(line: &str) -> bool {
    let s = line.to_ascii_lowercase();
    s.contains("permission requested")
        || s.contains("needs approval")
        || s.contains("awaiting input")
        || s.contains("waiting for user")
        || s.contains("type your own answer")
        || s.contains("choose an option")
}
