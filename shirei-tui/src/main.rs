use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct JobApiResponse {
    job: JobRecord,
}

#[derive(Debug, Deserialize)]
struct JobRecord {
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

#[derive(Parser, Debug)]
#[command(name = "shirei-tui", version, about = "Terminal UI for Shirei")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:7373")]
    base_url: String,
    #[arg(long)]
    token: Option<String>,
    #[arg(long)]
    state_dir: Option<String>,
    #[arg(long)]
    discover_path: Option<String>,
    #[arg(long, default_value_t = 2000)]
    refresh_ms: u64,
    #[arg(long)]
    focus_job_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectRow {
    project_id: String,
    root_path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct JobRow {
    project_id: String,
    job_id: String,
    name: String,
    status: String,
    needs_attention: i64,
    origin_repo_key: Option<String>,
    origin_pr_number: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct PermissionPrompt {
    id: String,
    permission: String,
    patterns: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct QuestionPrompt {
    id: String,
    header: String,
    question: String,
    options: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JobView {
    Running,
    WaitingInput,
    AwaitingReview,
    Actionable,
    All,
}

impl JobView {
    fn next(self) -> Self {
        match self {
            JobView::Running => JobView::WaitingInput,
            JobView::WaitingInput => JobView::AwaitingReview,
            JobView::AwaitingReview => JobView::Actionable,
            JobView::Actionable => JobView::All,
            JobView::All => JobView::Running,
        }
    }

    fn label(self) -> &'static str {
        match self {
            JobView::Running => "Running",
            JobView::WaitingInput => "Waiting Input",
            JobView::AwaitingReview => "Awaiting Review",
            JobView::Actionable => "All Actionable",
            JobView::All => "All Jobs",
        }
    }
}

struct ApiClient {
    base_url: String,
    client: reqwest::Client,
}

impl ApiClient {
    fn new(base_url: String, token: String) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).context("invalid auth token")?,
        );
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;
        Ok(Self { base_url, client })
    }

    async fn send_with_bootstrap<F>(&self, mut build: F) -> Result<reqwest::Response>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        match build().send().await {
            Ok(resp) => Ok(resp),
            Err(err) if is_daemon_unreachable(&err) => {
                start_daemon_in_background().context("failed to bootstrap shirei-daemon")?;
                self.wait_for_daemon_ready().await?;
                Ok(build().send().await?)
            }
            Err(err) => Err(err.into()),
        }
    }

    async fn wait_for_daemon_ready(&self) -> Result<()> {
        let health_url = format!("{}/api/v0.1/health", self.base_url);
        for _ in 0..20 {
            match self.client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(_) => {}
                Err(err) if is_daemon_unreachable(&err) => {}
                Err(err) => return Err(err).context("daemon reachable but health check failed"),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(anyhow!("daemon did not become healthy after bootstrap"))
    }

    async fn discover_project(&self, path: &str) -> Result<()> {
        self.send_with_bootstrap(|| {
            self.client
                .post(format!("{}/api/v0.1/projects/discover", self.base_url))
                .json(&serde_json::json!({"root_path": path}))
        })
        .await?
        .error_for_status()?;
        Ok(())
    }

    async fn projects(&self) -> Result<Vec<ProjectRow>> {
        let v = self
            .send_with_bootstrap(|| {
                self.client
                    .get(format!("{}/api/v0.1/projects", self.base_url))
            })
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;
        serde_json::from_value(
            v.get("projects")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        )
        .map_err(|e| anyhow!(e))
    }

    async fn jobs(&self, project_id: &str) -> Result<Vec<JobRow>> {
        let v = self
            .send_with_bootstrap(|| {
                self.client.get(format!(
                    "{}/api/v0.1/projects/{project_id}/jobs",
                    self.base_url
                ))
            })
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;
        serde_json::from_value(
            v.get("jobs")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        )
        .map_err(|e| anyhow!(e))
    }

    async fn spawn_job(&self, project_id: &str, name: &str, prompt: &str) -> Result<()> {
        self.send_with_bootstrap(|| {
            self.client
                .post(format!(
                    "{}/api/v0.1/projects/{project_id}/jobs",
                    self.base_url
                ))
                .json(&serde_json::json!({"name": name, "prompt": prompt}))
        })
        .await?
        .error_for_status()?;
        Ok(())
    }

    async fn logs(&self, job_id: &str) -> Result<Vec<String>> {
        let v = self
            .send_with_bootstrap(|| {
                self.client.get(format!(
                    "{}/api/v0.1/jobs/{job_id}/logs?tail_lines=200",
                    self.base_url
                ))
            })
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;
        serde_json::from_value(
            v.get("lines")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([])),
        )
        .map_err(|e| anyhow!(e))
    }

    async fn job(&self, job_id: &str) -> Result<JobRecord> {
        let v = self
            .send_with_bootstrap(|| {
                self.client
                    .get(format!("{}/api/v0.1/jobs/{job_id}", self.base_url))
            })
            .await?
            .error_for_status()?
            .json::<JobApiResponse>()
            .await?;
        Ok(v.job)
    }

    async fn attach_spec(&self, job_id: &str) -> Result<AttachRecord> {
        let v = self
            .send_with_bootstrap(|| {
                self.client
                    .post(format!("{}/api/v0.1/jobs/{job_id}/attach", self.base_url))
            })
            .await?
            .error_for_status()?
            .json::<AttachApiResponse>()
            .await?;
        Ok(v.attach)
    }

    async fn kill_job(&self, job_id: &str) -> Result<()> {
        self.send_with_bootstrap(|| {
            self.client
                .post(format!("{}/api/v0.1/jobs/{job_id}/kill", self.base_url))
        })
        .await?
        .error_for_status()?;
        Ok(())
    }

    async fn ack_job(&self, job_id: &str) -> Result<()> {
        self.send_with_bootstrap(|| {
            self.client
                .post(format!("{}/api/v0.1/jobs/{job_id}/ack", self.base_url))
        })
        .await?
        .error_for_status()?;
        Ok(())
    }

    async fn unack_job(&self, job_id: &str) -> Result<()> {
        self.send_with_bootstrap(|| {
            self.client
                .post(format!("{}/api/v0.1/jobs/{job_id}/unack", self.base_url))
        })
        .await?
        .error_for_status()?;
        Ok(())
    }

    async fn approve_job(&self, job_id: &str) -> Result<()> {
        self.send_with_bootstrap(|| {
            self.client
                .post(format!("{}/api/v0.1/jobs/{job_id}/approve", self.base_url))
        })
        .await?
        .error_for_status()?;
        Ok(())
    }

    async fn request_changes_job(&self, job_id: &str) -> Result<()> {
        self.send_with_bootstrap(|| {
            self.client
                .post(format!(
                    "{}/api/v0.1/jobs/{job_id}/request-changes",
                    self.base_url
                ))
                .json(&serde_json::json!({"feedback": null}))
        })
        .await?
        .error_for_status()?;
        Ok(())
    }

    async fn job_inputs(
        &self,
        job_id: &str,
    ) -> Result<(Vec<PermissionPrompt>, Vec<QuestionPrompt>)> {
        #[derive(Deserialize)]
        struct InputsResp {
            permissions: Vec<PermissionPrompt>,
            questions: Vec<QuestionPrompt>,
        }

        let v = self
            .send_with_bootstrap(|| {
                self.client
                    .get(format!("{}/api/v0.1/jobs/{job_id}/inputs", self.base_url))
            })
            .await?
            .error_for_status()?
            .json::<InputsResp>()
            .await?;
        Ok((v.permissions, v.questions))
    }

    async fn reply_permission(&self, job_id: &str, request_id: &str, reply: &str) -> Result<()> {
        self.send_with_bootstrap(|| {
            self.client
                .post(format!(
                    "{}/api/v0.1/jobs/{job_id}/permissions/{request_id}/reply",
                    self.base_url
                ))
                .json(&serde_json::json!({"reply": reply, "message": null}))
        })
        .await?
        .error_for_status()?;
        Ok(())
    }

    async fn reply_question(
        &self,
        job_id: &str,
        request_id: &str,
        answers: Vec<Vec<String>>,
    ) -> Result<()> {
        self.send_with_bootstrap(|| {
            self.client
                .post(format!(
                    "{}/api/v0.1/jobs/{job_id}/questions/{request_id}/reply",
                    self.base_url
                ))
                .json(&serde_json::json!({"answers": answers}))
        })
        .await?
        .error_for_status()?;
        Ok(())
    }

    async fn reject_question(&self, job_id: &str, request_id: &str) -> Result<()> {
        self.send_with_bootstrap(|| {
            self.client.post(format!(
                "{}/api/v0.1/jobs/{job_id}/questions/{request_id}/reject",
                self.base_url
            ))
        })
        .await?
        .error_for_status()?;
        Ok(())
    }

    async fn send_prompt(&self, job_id: &str, prompt: &str) -> Result<String> {
        let v = self
            .send_with_bootstrap(|| {
                self.client
                    .post(format!("{}/api/v0.1/jobs/{job_id}/prompt", self.base_url))
                    .json(&serde_json::json!({"prompt": prompt}))
            })
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;
        Ok(v.get("session_id")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string())
    }
}

struct App {
    api: ApiClient,
    discover_path: String,
    projects: Vec<ProjectRow>,
    jobs: Vec<JobRow>,
    logs: Vec<String>,
    pending_permissions: Vec<PermissionPrompt>,
    pending_questions: Vec<QuestionPrompt>,
    selected_job_id: Option<String>,
    job_view: JobView,
    search: String,
    search_mode: bool,
    help_mode: bool,
    project_picker_mode: bool,
    project_picker_query: String,
    project_picker_index: usize,
    prompt_mode: bool,
    prompt_buffer: String,
    startup_focus_job_id: Option<String>,
    log_scroll: usize,
    status: String,
}

impl App {
    async fn refresh_jobs(&mut self) {
        match self.api.projects().await {
            Ok(projects) => {
                self.projects = projects;
            }
            Err(e) => {
                self.status = format!("Refresh projects failed: {e}");
                return;
            }
        }

        let mut all_jobs = Vec::new();
        for project in &self.projects {
            match self.api.jobs(&project.project_id).await {
                Ok(mut jobs) => all_jobs.append(&mut jobs),
                Err(e) => {
                    self.status = format!("Refresh jobs failed for {}: {e}", project.project_id);
                    return;
                }
            }
        }
        self.jobs = all_jobs;
        if let Some(focus_job_id) = self.startup_focus_job_id.as_ref() {
            if self.jobs.iter().any(|j| j.job_id == *focus_job_id) {
                self.selected_job_id = Some(focus_job_id.clone());
                self.startup_focus_job_id = None;
            }
        }
        self.ensure_selected_job_visible();
        self.status = format!(
            "Loaded {} sessions across {} projects",
            self.jobs.len(),
            self.projects.len()
        );
    }

    fn filtered_project_indices(&self) -> Vec<usize> {
        self.projects
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                fuzzy_match(&self.project_picker_query, &p.project_id)
                    || fuzzy_match(&self.project_picker_query, &p.root_path)
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    fn clamp_project_picker_index(&mut self) {
        let visible = self.filtered_project_indices();
        if visible.is_empty() {
            self.project_picker_index = 0;
            return;
        }
        self.project_picker_index = self.project_picker_index.min(visible.len() - 1);
    }

    fn selected_project_for_spawn(&self) -> Option<&ProjectRow> {
        let visible = self.filtered_project_indices();
        let idx = *visible.get(self.project_picker_index)?;
        self.projects.get(idx)
    }

    fn open_project_picker(&mut self) {
        self.project_picker_mode = true;
        self.project_picker_query.clear();
        self.project_picker_index = 0;
        self.status =
            "Select a project for new session (type to filter, Enter to create)".to_string();
    }

    fn close_project_picker(&mut self, cancelled: bool) {
        self.project_picker_mode = false;
        self.project_picker_query.clear();
        self.project_picker_index = 0;
        if cancelled {
            self.status = "New session cancelled".to_string();
        }
    }

    fn open_prompt_input(&mut self) {
        if self.selected_job().is_none() {
            self.status = "No session selected".to_string();
            return;
        }
        self.prompt_mode = true;
        self.prompt_buffer.clear();
        self.status = "Compose prompt for selected session (Enter to send)".to_string();
    }

    fn close_prompt_input(&mut self, cancelled: bool) {
        self.prompt_mode = false;
        self.prompt_buffer.clear();
        if cancelled {
            self.status = "Prompt cancelled".to_string();
        }
    }

    async fn submit_prompt_input(&mut self) {
        let Some(job_id) = self.selected_job().map(|j| j.job_id.clone()) else {
            self.close_prompt_input(true);
            return;
        };
        let prompt = self.prompt_buffer.trim().to_string();
        if prompt.is_empty() {
            self.status = "Prompt cannot be empty".to_string();
            return;
        }

        match self.api.send_prompt(&job_id, &prompt).await {
            Ok(session_id) if session_id.is_empty() => {
                self.status = format!("Prompt sent to {job_id}");
            }
            Ok(session_id) => {
                self.status = format!("Prompt sent to {job_id} (session {session_id})");
            }
            Err(e) => {
                self.status = format!("Prompt send failed: {e}");
                return;
            }
        }
        self.close_prompt_input(false);
        self.refresh_jobs().await;
        self.refresh_logs().await;
        self.refresh_inputs().await;
    }

    async fn spawn_job_for_project(&mut self, project_id: &str) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let name = format!("tui-{ts}");
        let prompt = std::env::var("SHIREI_TUI_PROMPT")
            .unwrap_or_else(|_| "start an interactive session".to_string());
        match self.api.spawn_job(project_id, &name, &prompt).await {
            Ok(()) => self.status = format!("Spawned session {name}"),
            Err(e) => self.status = format!("Spawn failed: {e}"),
        }
        self.refresh_jobs().await;
        self.log_scroll = 0;
        self.refresh_logs().await;
        self.refresh_inputs().await;
    }

    async fn refresh_logs(&mut self) {
        let Some(job) = self.selected_job() else {
            self.logs.clear();
            self.log_scroll = 0;
            return;
        };
        match self.api.logs(&job.job_id).await {
            Ok(lines) => self.logs = lines,
            Err(e) => self.status = format!("Refresh logs failed: {e}"),
        }
    }

    async fn refresh_inputs(&mut self) {
        let Some(job) = self.selected_job() else {
            self.pending_permissions.clear();
            self.pending_questions.clear();
            return;
        };
        match self.api.job_inputs(&job.job_id).await {
            Ok((permissions, questions)) => {
                self.pending_permissions = permissions;
                self.pending_questions = questions;
            }
            Err(_) => {
                self.pending_permissions.clear();
                self.pending_questions.clear();
            }
        }
    }

    async fn allow_first_permission(&mut self, always: bool) {
        let Some(job_id) = self.selected_job().map(|j| j.job_id.clone()) else {
            return;
        };
        let Some(req) = self.pending_permissions.first() else {
            self.status = "No pending permission requests".to_string();
            return;
        };
        let reply = if always { "always" } else { "once" };
        match self.api.reply_permission(&job_id, &req.id, reply).await {
            Ok(()) => {
                self.status = format!("Permission {} replied: {reply}", req.id);
                self.refresh_inputs().await;
            }
            Err(e) => self.status = format!("Permission reply failed: {e}"),
        }
    }

    async fn reject_first_permission(&mut self) {
        let Some(job_id) = self.selected_job().map(|j| j.job_id.clone()) else {
            return;
        };
        let Some(req) = self.pending_permissions.first() else {
            self.status = "No pending permission requests".to_string();
            return;
        };
        match self.api.reply_permission(&job_id, &req.id, "reject").await {
            Ok(()) => {
                self.status = format!("Permission {} rejected", req.id);
                self.refresh_inputs().await;
            }
            Err(e) => self.status = format!("Permission reject failed: {e}"),
        }
    }

    async fn answer_first_question_option(&mut self, option_index: usize) {
        let Some(job_id) = self.selected_job().map(|j| j.job_id.clone()) else {
            return;
        };
        let Some(req) = self.pending_questions.first() else {
            self.status = "No pending question requests".to_string();
            return;
        };
        let Some(label) = req.options.get(option_index).cloned() else {
            self.status = format!("Question has no option {}", option_index + 1);
            return;
        };
        match self
            .api
            .reply_question(&job_id, &req.id, vec![vec![label.clone()]])
            .await
        {
            Ok(()) => {
                self.status = format!("Question {} answered: {}", req.id, label);
                self.refresh_inputs().await;
            }
            Err(e) => self.status = format!("Question answer failed: {e}"),
        }
    }

    async fn reject_first_question(&mut self) {
        let Some(job_id) = self.selected_job().map(|j| j.job_id.clone()) else {
            return;
        };
        let Some(req) = self.pending_questions.first() else {
            self.status = "No pending question requests".to_string();
            return;
        };
        match self.api.reject_question(&job_id, &req.id).await {
            Ok(()) => {
                self.status = format!("Question {} rejected", req.id);
                self.refresh_inputs().await;
            }
            Err(e) => self.status = format!("Question reject failed: {e}"),
        }
    }

    fn selected_job(&self) -> Option<&JobRow> {
        let id = self.selected_job_id.as_ref()?;
        self.jobs.iter().find(|j| &j.job_id == id)
    }

    fn filtered_job_indices(&self) -> Vec<usize> {
        self.jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| self.job_matches_view(j))
            .filter(|(_, j)| {
                let haystack = format!("{} {} {} {}", j.project_id, j.job_id, j.name, j.status);
                fuzzy_match(&self.search, &haystack)
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    fn ensure_selected_job_visible(&mut self) {
        if self.selected_job_id.as_ref().is_some_and(|id| {
            self.filtered_job_indices()
                .iter()
                .any(|idx| self.jobs[*idx].job_id == *id)
        }) {
            return;
        }
        self.selected_job_id = self
            .filtered_job_indices()
            .first()
            .map(|idx| self.jobs[*idx].job_id.clone());
    }

    fn count_running(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| j.status == "RUNNING" && j.needs_attention == 0)
            .count()
    }

    fn count_waiting_input(&self) -> usize {
        self.jobs.iter().filter(|j| is_waiting_input(j)).count()
    }

    fn count_awaiting_review(&self) -> usize {
        self.jobs.iter().filter(|j| is_awaiting_review(j)).count()
    }

    fn job_matches_view(&self, job: &JobRow) -> bool {
        match self.job_view {
            JobView::Running => job.status == "RUNNING" && job.needs_attention == 0,
            JobView::WaitingInput => is_waiting_input(job),
            JobView::AwaitingReview => is_awaiting_review(job),
            JobView::Actionable => {
                (job.status == "RUNNING" && job.needs_attention == 0)
                    || is_waiting_input(job)
                    || is_awaiting_review(job)
            }
            JobView::All => true,
        }
    }

    async fn move_selection(&mut self, delta: i32) {
        let visible = self.filtered_job_indices();
        if visible.is_empty() {
            return;
        }
        let current = self
            .selected_job_id
            .as_ref()
            .and_then(|id| visible.iter().position(|idx| self.jobs[*idx].job_id == *id))
            .unwrap_or(0) as i32;
        let next = (current + delta).clamp(0, visible.len() as i32 - 1) as usize;
        self.selected_job_id = Some(self.jobs[visible[next]].job_id.clone());
        self.log_scroll = 0;
        self.refresh_logs().await;
        self.refresh_inputs().await;
    }

    fn log_viewport_bounds(&self, viewport_height: usize) -> (usize, usize) {
        if self.logs.is_empty() {
            return (0, 0);
        }
        let height = viewport_height.max(1);
        let max_scroll = self.logs.len().saturating_sub(height);
        let scroll = self.log_scroll.min(max_scroll);
        let end = self.logs.len().saturating_sub(scroll);
        let start = end.saturating_sub(height);
        (start, end)
    }

    fn log_viewport_lines(&self, viewport_height: usize) -> Vec<Line<'static>> {
        let (start, end) = self.log_viewport_bounds(viewport_height);
        if start == end {
            return vec![];
        }
        self.logs[start..end]
            .iter()
            .map(|line| render_log_line(line))
            .collect()
    }

    async fn attach_selected_job_background(&mut self) {
        let Some(job_id) = self.selected_job().map(|j| j.job_id.clone()) else {
            return;
        };

        match self.api.attach_spec(&job_id).await {
            Ok(spec) => {
                if !std::path::Path::new(&spec.cwd).exists() {
                    self.status = format!("Attach cwd missing for {job_id}: {}", spec.cwd);
                    return;
                }

                let mut cmd = std::process::Command::new(&spec.cmd);
                cmd.args(&spec.argv)
                    .current_dir(&spec.cwd)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                match cmd.spawn() {
                    Ok(_) => {
                        self.status = format!("Attach launched in background for {job_id}");
                    }
                    Err(e) => {
                        self.status = format!("Attach launch failed for {job_id}: {e}");
                    }
                }
            }
            Err(e) => {
                self.status = format!("Attach failed for {job_id}: {e}");
            }
        }
    }

    async fn attach_selected_job_interactive(&mut self) {
        let Some(job_id) = self.selected_job().map(|j| j.job_id.clone()) else {
            return;
        };

        match self.api.attach_spec(&job_id).await {
            Ok(spec) => {
                if !std::path::Path::new(&spec.cwd).exists() {
                    self.status = format!("Attach cwd missing for {job_id}: {}", spec.cwd);
                    return;
                }

                let mut cmd = std::process::Command::new(&spec.cmd);
                cmd.args(&spec.argv)
                    .current_dir(&spec.cwd)
                    .stdin(std::process::Stdio::inherit())
                    .stdout(std::process::Stdio::inherit())
                    .stderr(std::process::Stdio::inherit());
                match run_interactive_command(cmd) {
                    Ok(status) if status.success() => {
                        self.status = format!(
                            "Detached from {job_id}; attach backend still running (use /q in OpenCode to detach)"
                        );
                    }
                    Ok(status) => {
                        self.status = format!(
                            "Attach exited non-zero for {job_id}: {}",
                            status.code().unwrap_or(-1)
                        );
                    }
                    Err(e) => {
                        self.status = format!("Attach failed for {job_id}: {e}");
                    }
                }
                self.refresh_jobs().await;
                self.refresh_logs().await;
                self.refresh_inputs().await;
            }
            Err(e) => {
                self.status = format!("Attach failed for {job_id}: {e}");
            }
        }
    }

    async fn edit_selected_job_background(&mut self) {
        let Some(job_id) = self.selected_job().map(|j| j.job_id.clone()) else {
            return;
        };

        let job = match self.api.job(&job_id).await {
            Ok(job) => job,
            Err(e) => {
                self.status = format!("Edit failed for {job_id}: {e}");
                return;
            }
        };

        let target = job.worktree_path.unwrap_or(job.cwd);
        if !std::path::Path::new(&target).exists() {
            self.status = format!("Edit path missing for {job_id}: {target}");
            return;
        }

        let editor_cmd = std::env::var("VISUAL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| {
                std::env::var("EDITOR")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            })
            .unwrap_or_else(|| "code".to_string());
        let escaped_path = shell_escape_single(&target);
        let command_line = if editor_cmd.contains("{path}") {
            editor_cmd.replace("{path}", &escaped_path)
        } else {
            format!("{editor_cmd} '{escaped_path}'")
        };

        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-lc")
            .arg(command_line)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match cmd.spawn() {
            Ok(_) => {
                self.status = format!("Editor launched in background for {job_id}");
            }
            Err(e) => {
                self.status = format!("Editor launch failed for {job_id}: {e}");
            }
        }
    }

    async fn edit_selected_job_interactive(&mut self) {
        let Some(job_id) = self.selected_job().map(|j| j.job_id.clone()) else {
            return;
        };

        let job = match self.api.job(&job_id).await {
            Ok(job) => job,
            Err(e) => {
                self.status = format!("Edit failed for {job_id}: {e}");
                return;
            }
        };

        let target = job.worktree_path.unwrap_or(job.cwd);
        if !std::path::Path::new(&target).exists() {
            self.status = format!("Edit path missing for {job_id}: {target}");
            return;
        }

        let editor_cmd = std::env::var("VISUAL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| {
                std::env::var("EDITOR")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            })
            .unwrap_or_else(|| "code".to_string());
        let escaped_path = shell_escape_single(&target);
        let command_line = if editor_cmd.contains("{path}") {
            editor_cmd.replace("{path}", &escaped_path)
        } else {
            format!("{editor_cmd} '{escaped_path}'")
        };

        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-lc")
            .arg(command_line)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
        match run_interactive_command(cmd) {
            Ok(status) if status.success() => {
                self.status = format!("Editor exited for {job_id}");
            }
            Ok(status) => {
                self.status = format!(
                    "Editor exited non-zero for {job_id}: {}",
                    status.code().unwrap_or(-1)
                );
            }
            Err(e) => {
                self.status = format!("Editor failed for {job_id}: {e}");
            }
        }
        self.refresh_jobs().await;
        self.refresh_logs().await;
        self.refresh_inputs().await;
    }

    fn open_selected_job_pr(&mut self) {
        let Some(job) = self.selected_job() else {
            return;
        };
        let Some(repo_key) = job.origin_repo_key.as_deref() else {
            self.status = format!("No PR linked to {}", job.job_id);
            return;
        };
        let Some(pr_number) = job.origin_pr_number else {
            self.status = format!("No PR number linked to {}", job.job_id);
            return;
        };

        let Some(url) = pr_web_url(repo_key, pr_number as u64) else {
            self.status = format!("Cannot build PR URL for {repo_key}");
            return;
        };

        let mut cmd = if cfg!(target_os = "macos") {
            let mut c = std::process::Command::new("open");
            c.arg(&url);
            c
        } else if cfg!(target_os = "windows") {
            let mut c = std::process::Command::new("cmd");
            c.arg("/C").arg("start").arg(&url);
            c
        } else {
            let mut c = std::process::Command::new("xdg-open");
            c.arg(&url);
            c
        };
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        match cmd.spawn() {
            Ok(_) => self.status = format!("Opened PR: {url}"),
            Err(e) => self.status = format!("Failed to open PR URL: {e}"),
        }
    }
}

fn status_color(job: &JobRow) -> Color {
    if is_waiting_input(job) {
        Color::Yellow
    } else if is_awaiting_review(job) {
        Color::LightMagenta
    } else if job.status == "RUNNING" {
        Color::Green
    } else {
        Color::DarkGray
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogKind {
    Agent,
    User,
    Tool,
    System,
    Output,
    Stderr,
}

struct ClassifiedLogLine<'a> {
    kind: LogKind,
    text: &'a str,
}

fn classify_log_line(line: &str) -> ClassifiedLogLine<'_> {
    if let Some(rest) = line.strip_prefix("[stderr] ") {
        return ClassifiedLogLine {
            kind: LogKind::Stderr,
            text: rest,
        };
    }

    if let Some(rest) = line.strip_prefix("[stdout] ") {
        return ClassifiedLogLine {
            kind: classify_semantic_kind(rest).unwrap_or(LogKind::Output),
            text: rest,
        };
    }

    ClassifiedLogLine {
        kind: classify_semantic_kind(line).unwrap_or(LogKind::System),
        text: line,
    }
}

fn classify_semantic_kind(line: &str) -> Option<LogKind> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Some(LogKind::System);
    }

    if let Some(kind) = kind_from_json_line(trimmed) {
        return Some(kind);
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("assistant") || lower.starts_with("agent") {
        return Some(LogKind::Agent);
    }
    if lower.starts_with("user") {
        return Some(LogKind::User);
    }
    if lower.starts_with("system") || lower.starts_with("[runner]") || lower.starts_with("runner") {
        return Some(LogKind::System);
    }
    if lower.starts_with("tool")
        || lower.contains("called the ")
        || lower.contains("tool call")
        || lower.contains("tool result")
        || lower.contains("\"recipient_name\":\"functions.")
    {
        return Some(LogKind::Tool);
    }
    None
}

fn kind_from_json_line(line: &str) -> Option<LogKind> {
    if !line.starts_with('{') {
        return None;
    }
    let v = serde_json::from_str::<serde_json::Value>(line).ok()?;
    kind_from_json_value(&v)
}

fn kind_from_json_value(v: &serde_json::Value) -> Option<LogKind> {
    if let Some(role) = v.get("role").and_then(|x| x.as_str()) {
        return kind_from_role(role);
    }

    if let Some(role) = v
        .get("message")
        .and_then(|m| m.get("role"))
        .and_then(|x| x.as_str())
    {
        return kind_from_role(role);
    }

    if let Some(t) = v.get("type").and_then(|x| x.as_str()) {
        return kind_from_type(t);
    }

    if v.get("tool_calls").is_some()
        || v.get("tool_call_id").is_some()
        || v.get("tool_name").is_some()
        || v.get("recipient_name").is_some()
    {
        return Some(LogKind::Tool);
    }

    None
}

fn kind_from_role(role: &str) -> Option<LogKind> {
    match role.to_ascii_lowercase().as_str() {
        "assistant" | "agent" => Some(LogKind::Agent),
        "user" => Some(LogKind::User),
        "system" => Some(LogKind::System),
        "tool" => Some(LogKind::Tool),
        _ => None,
    }
}

fn kind_from_type(t: &str) -> Option<LogKind> {
    let lower = t.to_ascii_lowercase();
    if lower.contains("assistant") || lower == "agent" {
        return Some(LogKind::Agent);
    }
    if lower.contains("user") {
        return Some(LogKind::User);
    }
    if lower.contains("system") || lower == "runner" {
        return Some(LogKind::System);
    }
    if lower.contains("tool") || lower.contains("function") {
        return Some(LogKind::Tool);
    }
    None
}

fn render_log_line(line: &str) -> Line<'static> {
    let classified = classify_log_line(line);
    let (label, label_color, text_color) = match classified.kind {
        LogKind::Agent => ("[agent]  ", Color::Cyan, Color::White),
        LogKind::User => ("[user]   ", Color::Yellow, Color::White),
        LogKind::Tool => ("[tool]   ", Color::Magenta, Color::White),
        LogKind::System => ("[system] ", Color::Green, Color::Gray),
        LogKind::Output => ("[output] ", Color::Blue, Color::Gray),
        LogKind::Stderr => ("[stderr] ", Color::Red, Color::LightRed),
    };

    Line::from(vec![
        Span::styled(label, Style::default().fg(label_color)),
        Span::styled(classified.text.to_string(), Style::default().fg(text_color)),
    ])
}

fn shell_escape_single(input: &str) -> String {
    input.replace('\'', "'\"'\"'")
}

fn run_interactive_command(mut cmd: std::process::Command) -> Result<std::process::ExitStatus> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    let result = cmd.status().context("failed launching interactive command");
    execute!(io::stdout(), EnterAlternateScreen)?;
    enable_raw_mode()?;
    result
}

fn start_daemon_in_background() -> Result<()> {
    std::process::Command::new("shirei-daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("failed to spawn shirei-daemon")?;
    Ok(())
}

fn is_daemon_unreachable(err: &reqwest::Error) -> bool {
    err.is_connect() || err.is_timeout()
}

fn pr_web_url(repo_key: &str, pr_number: u64) -> Option<String> {
    let (kind, repo_path) = repo_key.split_once(':')?;
    if repo_path.is_empty() || !repo_path.contains('/') {
        return None;
    }
    match kind {
        "github" => Some(format!("https://github.com/{repo_path}/pull/{pr_number}")),
        "gitlab" => Some(format!(
            "https://gitlab.com/{repo_path}/-/merge_requests/{pr_number}"
        )),
        _ => None,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

struct TuiGuard;

impl TuiGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TuiGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = match args.token {
        Some(t) => t,
        None => read_default_token(args.state_dir.as_deref()).context(
            "missing token; pass --token or create token in SHIREI_STATE_DIR or ~/.local/share/shirei",
        )?,
    };
    let discover_path = args.discover_path.unwrap_or_else(|| ".".to_string());
    let api = ApiClient::new(args.base_url, token)?;
    let refresh_interval = Duration::from_millis(args.refresh_ms.max(250));

    let _guard = TuiGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App {
        api,
        discover_path,
        projects: Vec::new(),
        jobs: Vec::new(),
        logs: Vec::new(),
        pending_permissions: Vec::new(),
        pending_questions: Vec::new(),
        selected_job_id: None,
        job_view: JobView::Actionable,
        search: String::new(),
        search_mode: false,
        help_mode: false,
        project_picker_mode: false,
        project_picker_query: String::new(),
        project_picker_index: 0,
        prompt_mode: false,
        prompt_buffer: String::new(),
        startup_focus_job_id: args.focus_job_id,
        log_scroll: 0,
        status: "?: help, /: search, v: filter, q: quit".to_string(),
    };

    app.refresh_jobs().await;
    app.refresh_logs().await;
    app.refresh_inputs().await;
    let mut last_refresh = Instant::now();

    loop {
        let job_visible = app.filtered_job_indices();

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Length(3),
                    Constraint::Min(5),
                    Constraint::Length(3),
                ])
                .split(f.area());

            let header = Line::from(vec![
                Span::styled(
                    " Shirei TUI ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  Session sidebar and live logs"),
            ]);
            f.render_widget(Paragraph::new(header), chunks[0]);

            let search_title = if app.search_mode {
                "Search *"
            } else {
                "Search"
            };
            let search = Paragraph::new(if app.search.is_empty() {
                "(type / to fuzzy-find sessions)".to_string()
            } else {
                app.search.clone()
            })
            .block(
                Block::default()
                    .title(format!(
                        "{} | view: {} | running:{} waiting:{} review:{}",
                        search_title,
                        app.job_view.label(),
                        app.count_running(),
                        app.count_waiting_input(),
                        app.count_awaiting_review()
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            );
            f.render_widget(search, chunks[1]);

            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(46), Constraint::Min(24)])
                .split(chunks[2]);

            let jobs_viewport_height = body[0].height.saturating_sub(2) as usize;
            let selected_visible_row = app.selected_job_id.as_ref().and_then(|selected_id| {
                job_visible
                    .iter()
                    .position(|idx| app.jobs[*idx].job_id == *selected_id)
            });
            let (job_list_start, job_list_end) = list_window_bounds(
                job_visible.len(),
                selected_visible_row,
                jobs_viewport_height,
                2,
            );
            let jobs_has_up = job_list_start > 0;
            let jobs_has_down = job_list_end < job_visible.len();
            let jobs_title = match (jobs_has_up, jobs_has_down) {
                (true, true) => "Sessions ↑ more ↓ more",
                (true, false) => "Sessions ↑ more",
                (false, true) => "Sessions ↓ more",
                (false, false) => "Sessions",
            };

            let job_items: Vec<ListItem> = job_visible[job_list_start..job_list_end]
                .iter()
                .map(|idx| {
                    let j = &app.jobs[*idx];
                    let marker = if Some(&j.job_id) == app.selected_job_id.as_ref() {
                        ">"
                    } else {
                        " "
                    };
                    let indicator = Span::styled("● ", Style::default().fg(status_color(j)));
                    let tag = if is_waiting_input(j) {
                        "WAIT"
                    } else if is_awaiting_review(j) {
                        "REVIEW"
                    } else if j.status == "RUNNING" {
                        "RUN"
                    } else {
                        "DONE"
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{} ", marker), Style::default().fg(Color::Cyan)),
                        indicator,
                        Span::styled(format!("{tag:<6}"), Style::default().fg(status_color(j))),
                        Span::raw(" "),
                        Span::styled(truncate(&j.name, 18), Style::default().add_modifier(Modifier::BOLD)),
                        Span::raw(" "),
                        Span::styled(truncate(&j.job_id, 10), Style::default().fg(Color::Gray)),
                    ]))
                })
                .collect();
            f.render_widget(
                List::new(job_items).block(
                    Block::default()
                        .title(jobs_title)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan)),
                ),
                body[0],
            );

            let log_viewport_height = body[1].height.saturating_sub(2) as usize;
            let log_lines = app.log_viewport_lines(log_viewport_height);
            let logs_title = if app.log_scroll == 0 {
                "Logs (live tail)".to_string()
            } else {
                format!("Logs (scrolled {} lines)", app.log_scroll)
            };
            let logs = Paragraph::new(ratatui::text::Text::from(log_lines))
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .title(format!(
                            "{logs_title} | legend: agent/user/tool/system/output/stderr"
                        ))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Blue)),
                );
            f.render_widget(logs, body[1]);

            let footer = Paragraph::new(format!(
                "{} | keys: ? help, q quit, / search, v filter, r refresh, n new, p prompt, t attach, e edit, y approve, c changes",
                app.status
            ))
            .style(
                Style::default()
                    .fg(Color::White)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
            f.render_widget(footer, chunks[3]);

            if app.project_picker_mode {
                let area = centered_rect(72, 74, f.area());
                f.render_widget(Clear, area);
                let sections = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(3), Constraint::Min(5), Constraint::Length(1)])
                    .split(area);

                let picker_search = Paragraph::new(if app.project_picker_query.is_empty() {
                    "(type to fuzzy-find projects)".to_string()
                } else {
                    app.project_picker_query.clone()
                })
                .block(
                    Block::default()
                        .title("Create Session: Project Filter")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan)),
                );
                f.render_widget(picker_search, sections[0]);

                let visible = app.filtered_project_indices();
                let picker_viewport_height = sections[1].height.saturating_sub(2) as usize;
                let (picker_start, picker_end) = list_window_bounds(
                    visible.len(),
                    Some(app.project_picker_index),
                    picker_viewport_height,
                    2,
                );
                let picker_has_up = picker_start > 0;
                let picker_has_down = picker_end < visible.len();
                let picker_title = match (picker_has_up, picker_has_down) {
                    (true, true) => "Projects ↑ more ↓ more",
                    (true, false) => "Projects ↑ more",
                    (false, true) => "Projects ↓ more",
                    (false, false) => "Projects",
                };
                let picker_items = if visible.is_empty() {
                    vec![ListItem::new(Line::from(Span::styled(
                        "No matching projects",
                        Style::default().fg(Color::DarkGray),
                    )))]
                } else {
                    visible[picker_start..picker_end]
                        .iter()
                        .enumerate()
                        .map(|(row, idx)| {
                            let p = &app.projects[*idx];
                            let absolute_row = picker_start + row;
                            let marker = if absolute_row == app.project_picker_index {
                                ">"
                            } else {
                                " "
                            };
                            ListItem::new(Line::from(vec![
                                Span::styled(format!("{} ", marker), Style::default().fg(Color::Cyan)),
                                Span::styled(
                                    truncate(&p.project_id, 30),
                                    Style::default().add_modifier(Modifier::BOLD),
                                ),
                                Span::raw("  "),
                                Span::styled(truncate(&p.root_path, 45), Style::default().fg(Color::Gray)),
                            ]))
                        })
                        .collect()
                };

                f.render_widget(
                    List::new(picker_items).block(
                        Block::default()
                            .title(picker_title)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Cyan)),
                    ),
                    sections[1],
                );

                f.render_widget(
                    Paragraph::new("Enter create session | Esc cancel | Up/Down select")
                        .style(Style::default().fg(Color::Gray)),
                    sections[2],
                );
            }

            if app.prompt_mode {
                let area = centered_rect(72, 22, f.area());
                f.render_widget(Clear, area);
                let prompt = Paragraph::new(if app.prompt_buffer.is_empty() {
                    "(type prompt to send to selected session)".to_string()
                } else {
                    app.prompt_buffer.clone()
                })
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .title("Send Prompt (Enter send | Esc cancel)")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Cyan)),
                );
                f.render_widget(prompt, area);
            }

            if app.help_mode {
                let area = centered_rect(78, 80, f.area());
                f.render_widget(Clear, area);
                let help = Paragraph::new(ratatui::text::Text::from(action_help_lines()))
                    .wrap(Wrap { trim: false })
                    .block(
                        Block::default()
                            .title("Actions (press ? or Esc to close)")
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(Color::Cyan)),
                    );
                f.render_widget(help, area);
            }
        })?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(k) = event::read()? {
                if app.project_picker_mode {
                    match k.code {
                        KeyCode::Esc => app.close_project_picker(true),
                        KeyCode::Up => {
                            app.project_picker_index = app.project_picker_index.saturating_sub(1);
                        }
                        KeyCode::Down => {
                            app.project_picker_index = app.project_picker_index.saturating_add(1);
                            app.clamp_project_picker_index();
                        }
                        KeyCode::Backspace => {
                            app.project_picker_query.pop();
                            app.clamp_project_picker_index();
                        }
                        KeyCode::Char(c) => {
                            app.project_picker_query.push(c);
                            app.project_picker_index = 0;
                            app.clamp_project_picker_index();
                        }
                        KeyCode::Enter => {
                            let project_id = app
                                .selected_project_for_spawn()
                                .map(|p| p.project_id.clone());
                            if let Some(project_id) = project_id {
                                app.close_project_picker(false);
                                app.spawn_job_for_project(&project_id).await;
                                last_refresh = Instant::now();
                            } else {
                                app.status = "No matching project selected".to_string();
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                if app.help_mode {
                    match k.code {
                        KeyCode::Esc | KeyCode::Char('?') => {
                            app.help_mode = false;
                            app.status = "Closed action help".to_string();
                        }
                        _ => {}
                    }
                    continue;
                }

                if app.search_mode {
                    match k.code {
                        KeyCode::Esc | KeyCode::Enter => {
                            app.search_mode = false;
                            app.ensure_selected_job_visible();
                            app.refresh_logs().await;
                            app.refresh_inputs().await;
                        }
                        KeyCode::Backspace => {
                            app.search.pop();
                            app.ensure_selected_job_visible();
                            app.refresh_logs().await;
                            app.refresh_inputs().await;
                        }
                        KeyCode::Char(c) => {
                            app.search.push(c);
                            app.ensure_selected_job_visible();
                            app.refresh_logs().await;
                            app.refresh_inputs().await;
                        }
                        _ => {}
                    }
                    continue;
                }

                if app.prompt_mode {
                    match k.code {
                        KeyCode::Esc => app.close_prompt_input(true),
                        KeyCode::Enter => {
                            app.submit_prompt_input().await;
                            last_refresh = Instant::now();
                        }
                        KeyCode::Backspace => {
                            app.prompt_buffer.pop();
                        }
                        KeyCode::Char(c) => {
                            app.prompt_buffer.push(c);
                        }
                        _ => {}
                    }
                    continue;
                }

                match k.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('?') => {
                        app.help_mode = true;
                        app.status = "Action help opened".to_string();
                    }
                    KeyCode::Char('/') => {
                        app.search_mode = true;
                    }
                    KeyCode::Esc => {
                        app.search.clear();
                        app.ensure_selected_job_visible();
                        app.log_scroll = 0;
                        app.refresh_logs().await;
                        app.refresh_inputs().await;
                    }
                    KeyCode::Char('v') => {
                        app.job_view = app.job_view.next();
                        app.ensure_selected_job_visible();
                        app.log_scroll = 0;
                        app.refresh_logs().await;
                        app.refresh_inputs().await;
                        app.status = format!("Session view: {}", app.job_view.label());
                    }
                    KeyCode::Char('d') => {
                        match app.api.discover_project(&app.discover_path).await {
                            Ok(()) => app.status = format!("Discovered {}", app.discover_path),
                            Err(e) => app.status = format!("Discover failed: {e}"),
                        }
                        app.refresh_jobs().await;
                    }
                    KeyCode::Char('r') => {
                        app.refresh_jobs().await;
                        app.refresh_logs().await;
                        app.refresh_inputs().await;
                        last_refresh = Instant::now();
                    }
                    KeyCode::Char('n') => {
                        if app.projects.is_empty() {
                            app.status =
                                "No projects available; press d to discover one".to_string();
                        } else {
                            app.open_project_picker();
                        }
                    }
                    KeyCode::Char('p') => {
                        app.open_prompt_input();
                    }
                    KeyCode::Char('a') => {
                        if let Some(job_id) = app.selected_job().map(|j| j.job_id.clone()) {
                            match app.api.ack_job(&job_id).await {
                                Ok(()) => app.status = format!("Acknowledged {job_id}"),
                                Err(e) => app.status = format!("Ack failed: {e}"),
                            }
                            app.refresh_jobs().await;
                            app.log_scroll = 0;
                            app.refresh_logs().await;
                            app.refresh_inputs().await;
                        }
                    }
                    KeyCode::Char('u') => {
                        if let Some(job_id) = app.selected_job().map(|j| j.job_id.clone()) {
                            match app.api.unack_job(&job_id).await {
                                Ok(()) => app.status = format!("Unacknowledged {job_id}"),
                                Err(e) => app.status = format!("Unack failed: {e}"),
                            }
                            app.refresh_jobs().await;
                            app.log_scroll = 0;
                            app.refresh_logs().await;
                            app.refresh_inputs().await;
                        }
                    }
                    KeyCode::Char('k') => {
                        if let Some(job_id) = app.selected_job().map(|j| j.job_id.clone()) {
                            match app.api.kill_job(&job_id).await {
                                Ok(()) => app.status = format!("Killed {job_id}"),
                                Err(e) => app.status = format!("Kill failed: {e}"),
                            }
                            app.refresh_jobs().await;
                            app.log_scroll = 0;
                            app.refresh_logs().await;
                            app.refresh_inputs().await;
                        }
                    }
                    KeyCode::Char('y') => {
                        if let Some(job_id) = app.selected_job().map(|j| j.job_id.clone()) {
                            match app.api.approve_job(&job_id).await {
                                Ok(()) => app.status = format!("Approved {job_id}"),
                                Err(e) => app.status = format!("Approve failed: {e}"),
                            }
                            app.refresh_jobs().await;
                            app.log_scroll = 0;
                            app.refresh_logs().await;
                            app.refresh_inputs().await;
                        }
                    }
                    KeyCode::Char('c') => {
                        if let Some(job_id) = app.selected_job().map(|j| j.job_id.clone()) {
                            match app.api.request_changes_job(&job_id).await {
                                Ok(()) => {
                                    app.status = format!("Requested changes and reran {job_id}")
                                }
                                Err(e) => app.status = format!("Request changes failed: {e}"),
                            }
                            app.refresh_jobs().await;
                            app.log_scroll = 0;
                            app.refresh_logs().await;
                            app.refresh_inputs().await;
                            last_refresh = Instant::now();
                        }
                    }
                    KeyCode::Char('g') => {
                        app.allow_first_permission(false).await;
                    }
                    KeyCode::Char('G') => {
                        app.allow_first_permission(true).await;
                    }
                    KeyCode::Char('x') => {
                        app.reject_first_permission().await;
                    }
                    KeyCode::Char('0') => {
                        app.reject_first_question().await;
                    }
                    KeyCode::Char('1') => app.answer_first_question_option(0).await,
                    KeyCode::Char('2') => app.answer_first_question_option(1).await,
                    KeyCode::Char('3') => app.answer_first_question_option(2).await,
                    KeyCode::Char('4') => app.answer_first_question_option(3).await,
                    KeyCode::Char('5') => app.answer_first_question_option(4).await,
                    KeyCode::Char('6') => app.answer_first_question_option(5).await,
                    KeyCode::Char('7') => app.answer_first_question_option(6).await,
                    KeyCode::Char('8') => app.answer_first_question_option(7).await,
                    KeyCode::Char('9') => app.answer_first_question_option(8).await,
                    KeyCode::Char('t') => app.attach_selected_job_interactive().await,
                    KeyCode::Char('e') => app.edit_selected_job_interactive().await,
                    KeyCode::Char('T') => app.attach_selected_job_background().await,
                    KeyCode::Char('E') => app.edit_selected_job_background().await,
                    KeyCode::Char('o') => {
                        app.open_selected_job_pr();
                    }
                    KeyCode::Up => {
                        app.move_selection(-1).await;
                    }
                    KeyCode::Down => {
                        app.move_selection(1).await;
                    }
                    KeyCode::Enter => {
                        app.log_scroll = 0;
                        app.refresh_logs().await;
                        app.refresh_inputs().await;
                    }
                    _ => {}
                }
            }
        } else if last_refresh.elapsed() >= refresh_interval {
            app.refresh_jobs().await;
            app.refresh_logs().await;
            app.refresh_inputs().await;
            last_refresh = Instant::now();
        }
    }

    Ok(())
}

fn is_waiting_input(job: &JobRow) -> bool {
    let s = job.status.to_ascii_uppercase();
    (job.status == "RUNNING" && job.needs_attention > 0)
        || s == "WAITING_INPUT"
        || s == "WAIT_INPUT"
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn action_help_lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "Navigation",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("up/down  move selection"),
        Line::from("/  open fuzzy search mode"),
        Line::from("v  cycle session state filter"),
        Line::from("r  refresh sessions/logs"),
        Line::from(""),
        Line::from(Span::styled(
            "Session Actions",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("n  create new session (fuzzy-pick project)"),
        Line::from("p  send prompt to selected session"),
        Line::from("t  attach OpenCode interactively (use /q there to detach)"),
        Line::from("e  open editor interactively in selected session"),
        Line::from("T  attach OpenCode in background"),
        Line::from("E  open editor in background"),
        Line::from("o  open linked PR/MR in browser"),
        Line::from("k  kill selected session"),
        Line::from(""),
        Line::from(Span::styled(
            "Review & Input",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("y  approve selected session"),
        Line::from("c  request changes and rerun"),
        Line::from("g/G  allow first permission (once/always)"),
        Line::from("x  reject first permission"),
        Line::from("1-9  answer first pending question by option index"),
        Line::from("0  reject first pending question"),
        Line::from("a/u  ack or unack selected session"),
        Line::from(""),
        Line::from(Span::styled(
            "Global",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("? or Esc  close this help"),
        Line::from("q  quit TUI"),
    ]
}

fn is_awaiting_review(job: &JobRow) -> bool {
    if job.status == "AWAITING_REVIEW" {
        return true;
    }
    if job.needs_attention <= 0 {
        return false;
    }
    job.status != "RUNNING" || !is_waiting_input(job)
}

fn fuzzy_match(query: &str, haystack: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    let q_lower = q.to_ascii_lowercase();
    let mut q_chars = q_lower.chars();
    let mut current = match q_chars.next() {
        Some(c) => c,
        None => return true,
    };
    for c in haystack.to_ascii_lowercase().chars() {
        if c == current {
            if let Some(next) = q_chars.next() {
                current = next;
            } else {
                return true;
            }
        }
    }
    false
}

fn list_window_bounds(
    total_items: usize,
    selected_index: Option<usize>,
    viewport_height: usize,
    margin: usize,
) -> (usize, usize) {
    if total_items == 0 {
        return (0, 0);
    }
    if viewport_height == 0 || total_items <= viewport_height {
        return (0, total_items);
    }

    let selected = selected_index
        .unwrap_or(0)
        .min(total_items.saturating_sub(1));
    let mut start = 0_usize;
    let mut end = viewport_height;

    if selected + margin >= end {
        start = (selected + margin + 1).saturating_sub(viewport_height);
        end = start + viewport_height;
    }

    if selected < start + margin {
        start = selected.saturating_sub(margin);
        end = start + viewport_height;
    }

    if end > total_items {
        end = total_items;
        start = end.saturating_sub(viewport_height);
    }

    (start, end)
}

fn token_candidates(state_dir: Option<&str>) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    if let Some(dir) = state_dir {
        paths.push(PathBuf::from(dir).join("token"));
    } else if let Ok(dir) = std::env::var("SHIREI_STATE_DIR") {
        paths.push(PathBuf::from(dir).join("token"));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME not set"))?;
    paths.push(home.join(".local/share/shirei/token"));
    Ok(paths)
}

fn read_default_token(state_dir: Option<&str>) -> Result<String> {
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
    Err(anyhow!("no token file found; tried: {tried}"))
}

#[cfg(test)]
mod tests {
    use super::{
        classify_log_line, fuzzy_match, is_awaiting_review, is_waiting_input, list_window_bounds,
        token_candidates, JobRow, LogKind,
    };

    #[test]
    fn token_candidates_include_state_dir_first() {
        let candidates = token_candidates(Some("/tmp/shirei-state")).expect("token paths");
        assert_eq!(candidates[0].to_string_lossy(), "/tmp/shirei-state/token");
    }

    #[test]
    fn fuzzy_match_subsequence() {
        assert!(fuzzy_match("obsr", "observability-retry"));
        assert!(fuzzy_match("jb84", "job_84a28da3-a6ce"));
        assert!(!fuzzy_match("zzq", "observability-retry"));
    }

    #[test]
    fn job_bucket_helpers() {
        let running = JobRow {
            project_id: "p".to_string(),
            job_id: "a".to_string(),
            name: "n".to_string(),
            status: "RUNNING".to_string(),
            needs_attention: 0,
            origin_repo_key: None,
            origin_pr_number: None,
        };
        let waiting = JobRow {
            project_id: "p".to_string(),
            job_id: "b".to_string(),
            name: "n".to_string(),
            status: "RUNNING".to_string(),
            needs_attention: 1,
            origin_repo_key: None,
            origin_pr_number: None,
        };
        let review = JobRow {
            project_id: "p".to_string(),
            job_id: "c".to_string(),
            name: "n".to_string(),
            status: "DONE_UNREAD".to_string(),
            needs_attention: 1,
            origin_repo_key: Some("github:a/b".to_string()),
            origin_pr_number: Some(1),
        };
        assert!(!is_waiting_input(&running));
        assert!(is_waiting_input(&waiting));
        assert!(is_awaiting_review(&review));
    }

    #[test]
    fn classify_richer_log_semantics() {
        let c = classify_log_line("[stderr] failed to parse response");
        assert_eq!(c.kind, LogKind::Stderr);

        let c = classify_log_line("[stdout] assistant: generated a patch");
        assert_eq!(c.kind, LogKind::Agent);

        let c = classify_log_line("[stdout] user asked for a retry");
        assert_eq!(c.kind, LogKind::User);

        let c = classify_log_line("[stdout] Called the Read tool with the following input");
        assert_eq!(c.kind, LogKind::Tool);

        let c = classify_log_line("[stdout] [runner] opencode run \"prompt\"");
        assert_eq!(c.kind, LogKind::System);

        let c = classify_log_line("[stdout] plain text output from process");
        assert_eq!(c.kind, LogKind::Output);
    }

    #[test]
    fn classify_json_role_and_type_lines() {
        let c = classify_log_line("[stdout] {\"role\":\"assistant\",\"content\":\"ok\"}");
        assert_eq!(c.kind, LogKind::Agent);

        let c = classify_log_line("[stdout] {\"message\":{\"role\":\"user\"}}");
        assert_eq!(c.kind, LogKind::User);

        let c = classify_log_line("[stdout] {\"type\":\"tool_call\",\"name\":\"read\"}");
        assert_eq!(c.kind, LogKind::Tool);

        let c = classify_log_line("[stdout] {\"type\":\"system\",\"content\":\"boot\"}");
        assert_eq!(c.kind, LogKind::System);
    }

    #[test]
    fn list_window_keeps_selection_in_view() {
        let (start, end) = list_window_bounds(30, Some(12), 8, 2);
        assert_eq!((start, end), (7, 15));

        let (start, end) = list_window_bounds(30, Some(29), 8, 2);
        assert_eq!((start, end), (22, 30));
    }
}
