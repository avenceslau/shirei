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
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::prelude::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap};
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
}

#[derive(Debug, Clone, Deserialize)]
struct ProjectRow {
    project_id: String,
    root_path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct JobRow {
    job_id: String,
    name: String,
    status: String,
    needs_attention: i64,
    origin_repo_key: Option<String>,
    origin_pr_number: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Focus {
    Projects,
    Jobs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JobView {
    Running,
    WaitingInput,
    AwaitingReview,
    Actionable,
}

impl JobView {
    fn next(self) -> Self {
        match self {
            JobView::Running => JobView::WaitingInput,
            JobView::WaitingInput => JobView::AwaitingReview,
            JobView::AwaitingReview => JobView::Actionable,
            JobView::Actionable => JobView::Running,
        }
    }

    fn label(self) -> &'static str {
        match self {
            JobView::Running => "Running",
            JobView::WaitingInput => "Waiting Input",
            JobView::AwaitingReview => "Awaiting Review",
            JobView::Actionable => "All Actionable",
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

    async fn discover_project(&self, path: &str) -> Result<()> {
        self.client
            .post(format!("{}/api/v0.1/projects/discover", self.base_url))
            .json(&serde_json::json!({"root_path": path}))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn projects(&self) -> Result<Vec<ProjectRow>> {
        let v = self
            .client
            .get(format!("{}/api/v0.1/projects", self.base_url))
            .send()
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
            .client
            .get(format!(
                "{}/api/v0.1/projects/{project_id}/jobs",
                self.base_url
            ))
            .send()
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
        self.client
            .post(format!(
                "{}/api/v0.1/projects/{project_id}/jobs",
                self.base_url
            ))
            .json(&serde_json::json!({"name": name, "prompt": prompt}))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn logs(&self, job_id: &str) -> Result<Vec<String>> {
        let v = self
            .client
            .get(format!(
                "{}/api/v0.1/jobs/{job_id}/logs?tail_lines=200",
                self.base_url
            ))
            .send()
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
            .client
            .get(format!("{}/api/v0.1/jobs/{job_id}", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json::<JobApiResponse>()
            .await?;
        Ok(v.job)
    }

    async fn attach_spec(&self, job_id: &str) -> Result<AttachRecord> {
        let v = self
            .client
            .post(format!("{}/api/v0.1/jobs/{job_id}/attach", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json::<AttachApiResponse>()
            .await?;
        Ok(v.attach)
    }

    async fn kill_job(&self, job_id: &str) -> Result<()> {
        self.client
            .post(format!("{}/api/v0.1/jobs/{job_id}/kill", self.base_url))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn ack_job(&self, job_id: &str) -> Result<()> {
        self.client
            .post(format!("{}/api/v0.1/jobs/{job_id}/ack", self.base_url))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn unack_job(&self, job_id: &str) -> Result<()> {
        self.client
            .post(format!("{}/api/v0.1/jobs/{job_id}/unack", self.base_url))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn approve_job(&self, job_id: &str) -> Result<()> {
        self.client
            .post(format!("{}/api/v0.1/jobs/{job_id}/approve", self.base_url))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn request_changes_job(&self, job_id: &str) -> Result<()> {
        self.client
            .post(format!(
                "{}/api/v0.1/jobs/{job_id}/request-changes",
                self.base_url
            ))
            .json(&serde_json::json!({"feedback": null}))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

struct App {
    api: ApiClient,
    discover_path: String,
    projects: Vec<ProjectRow>,
    jobs: Vec<JobRow>,
    logs: Vec<String>,
    selected_project_id: Option<String>,
    selected_job_id: Option<String>,
    focus: Focus,
    job_view: JobView,
    search: String,
    search_mode: bool,
    logs_maximized: bool,
    log_scroll: usize,
    status: String,
}

impl App {
    async fn refresh_projects(&mut self) {
        match self.api.projects().await {
            Ok(projects) => {
                self.projects = projects;
                if self
                    .selected_project_id
                    .as_ref()
                    .is_some_and(|id| self.projects.iter().any(|p| &p.project_id == id))
                {
                    return;
                }
                self.selected_project_id = self.projects.first().map(|p| p.project_id.clone());
                self.status = format!("Loaded {} projects", self.projects.len());
            }
            Err(e) => self.status = format!("Refresh projects failed: {e}"),
        }
    }

    async fn refresh_jobs(&mut self) {
        let Some(project) = self.selected_project() else {
            self.jobs.clear();
            self.selected_job_id = None;
            return;
        };
        match self.api.jobs(&project.project_id).await {
            Ok(jobs) => {
                self.jobs = jobs;
                self.ensure_selected_job_visible();
                self.status = format!(
                    "Jobs: {} running, {} waiting input, {} awaiting review",
                    self.count_running(),
                    self.count_waiting_input(),
                    self.count_awaiting_review()
                );
            }
            Err(e) => self.status = format!("Refresh jobs failed: {e}"),
        }
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

    fn selected_project(&self) -> Option<&ProjectRow> {
        let id = self.selected_project_id.as_ref()?;
        self.projects.iter().find(|p| &p.project_id == id)
    }

    fn selected_job(&self) -> Option<&JobRow> {
        let id = self.selected_job_id.as_ref()?;
        self.jobs.iter().find(|j| &j.job_id == id)
    }

    fn filtered_project_indices(&self) -> Vec<usize> {
        self.projects
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                fuzzy_match(&self.search, &p.project_id) || fuzzy_match(&self.search, &p.root_path)
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    fn filtered_job_indices(&self) -> Vec<usize> {
        self.jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| self.job_matches_view(j))
            .filter(|(_, j)| {
                let haystack = format!("{} {} {}", j.job_id, j.name, j.status);
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
        }
    }

    async fn move_selection(&mut self, delta: i32) {
        match self.focus {
            Focus::Projects => {
                let visible = self.filtered_project_indices();
                if visible.is_empty() {
                    return;
                }
                let current = self
                    .selected_project_id
                    .as_ref()
                    .and_then(|id| {
                        visible
                            .iter()
                            .position(|idx| self.projects[*idx].project_id == *id)
                    })
                    .unwrap_or(0) as i32;
                let next = (current + delta).clamp(0, visible.len() as i32 - 1) as usize;
                self.selected_project_id = Some(self.projects[visible[next]].project_id.clone());
                self.refresh_jobs().await;
                self.log_scroll = 0;
                self.refresh_logs().await;
            }
            Focus::Jobs => {
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
            }
        }
    }

    fn scroll_logs_up(&mut self, lines: usize, viewport_height: usize) {
        if self.logs.is_empty() {
            self.log_scroll = 0;
            return;
        }
        let max_scroll = self.logs.len().saturating_sub(viewport_height.max(1));
        self.log_scroll = (self.log_scroll + lines).min(max_scroll);
    }

    fn scroll_logs_down(&mut self, lines: usize) {
        self.log_scroll = self.log_scroll.saturating_sub(lines);
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

fn render_log_line(line: &str) -> Line<'static> {
    if let Some(rest) = line.strip_prefix("[stderr] ") {
        return Line::from(vec![
            Span::styled("[stderr] ", Style::default().fg(Color::Red)),
            Span::styled(rest.to_string(), Style::default().fg(Color::LightRed)),
        ]);
    }

    if let Some(rest) = line.strip_prefix("[stdout] ") {
        if looks_like_agent_message(rest) {
            return Line::from(vec![
                Span::styled("[agent]  ", Style::default().fg(Color::Cyan)),
                Span::styled(rest.to_string(), Style::default().fg(Color::White)),
            ]);
        }
        return Line::from(vec![
            Span::styled("[output] ", Style::default().fg(Color::Blue)),
            Span::styled(rest.to_string(), Style::default().fg(Color::Gray)),
        ]);
    }

    Line::from(Span::styled(
        line.to_string(),
        Style::default().fg(Color::DarkGray),
    ))
}

fn looks_like_agent_message(line: &str) -> bool {
    let lower = line.trim_start().to_ascii_lowercase();
    lower.starts_with("assistant")
        || lower.starts_with("agent")
        || lower.contains("\"role\":\"assistant\"")
        || lower.contains("\"type\":\"assistant\"")
}

fn shell_escape_single(input: &str) -> String {
    input.replace('\'', "'\"'\"'")
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
        selected_project_id: None,
        selected_job_id: None,
        focus: Focus::Jobs,
        job_view: JobView::Actionable,
        search: String::new(),
        search_mode: false,
        logs_maximized: false,
        log_scroll: 0,
        status: "v: view, /: fuzzy search, tab: focus, q: quit".to_string(),
    };

    app.refresh_projects().await;
    app.refresh_jobs().await;
    app.refresh_logs().await;
    let mut last_refresh = Instant::now();

    loop {
        let project_visible = app.filtered_project_indices();
        let job_visible = app.filtered_job_indices();

        terminal.draw(|f| {
            let chunks = if app.logs_maximized {
                Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1),
                        Constraint::Length(3),
                        Constraint::Min(5),
                        Constraint::Length(3),
                    ])
                    .split(f.area())
            } else {
                Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(1),
                        Constraint::Length(3),
                        Constraint::Percentage(55),
                        Constraint::Percentage(37),
                        Constraint::Length(3),
                    ])
                    .split(f.area())
            };

            let header = Line::from(vec![
                Span::styled(" Shirei TUI ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw("  Actionable sessions and reviews"),
            ]);
            f.render_widget(Paragraph::new(header), chunks[0]);

            let search_title = if app.search_mode { "Search *" } else { "Search" };
            let search = Paragraph::new(if app.search.is_empty() {
                "(type / to fuzzy-find sessions/projects)".to_string()
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

            if !app.logs_maximized {
                let tab_titles = ["Running", "Waiting Input", "Awaiting Review", "All Actionable"]
                    .into_iter()
                    .map(Line::from)
                    .collect::<Vec<_>>();
                let tabs = Tabs::new(tab_titles)
                    .select(match app.job_view {
                        JobView::Running => 0,
                        JobView::WaitingInput => 1,
                        JobView::AwaitingReview => 2,
                        JobView::Actionable => 3,
                    })
                    .style(Style::default().fg(Color::Gray))
                    .highlight_style(
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    );
                let top_sections = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(1), Constraint::Min(0)])
                    .split(chunks[2]);
                f.render_widget(tabs, top_sections[0]);

                let top = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                    .split(top_sections[1]);

                let project_items: Vec<ListItem> = project_visible
                    .iter()
                    .map(|idx| {
                        let p = &app.projects[*idx];
                        let marker = if Some(&p.project_id) == app.selected_project_id.as_ref() {
                            ">"
                        } else {
                            " "
                        };
                        ListItem::new(Line::from(vec![
                            Span::styled(format!("{} ", marker), Style::default().fg(Color::Cyan)),
                            Span::styled(
                                truncate(&p.project_id, 20),
                                Style::default().add_modifier(Modifier::BOLD),
                            ),
                            Span::raw("  "),
                            Span::styled(truncate(&p.root_path, 38), Style::default().fg(Color::Gray)),
                        ]))
                    })
                    .collect();
                let project_title = if app.focus == Focus::Projects {
                    "Projects *"
                } else {
                    "Projects"
                };
                f.render_widget(
                    List::new(project_items).block(
                        Block::default()
                            .title(project_title)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(if app.focus == Focus::Projects {
                                Color::Cyan
                            } else {
                                Color::DarkGray
                            })),
                    ),
                    top[0],
                );

                let job_items: Vec<ListItem> = job_visible
                    .iter()
                    .map(|idx| {
                        let j = &app.jobs[*idx];
                        let marker = if Some(&j.job_id) == app.selected_job_id.as_ref() {
                            ">"
                        } else {
                            " "
                        };
                        let tag = if is_waiting_input(j) {
                            "WAIT_INPUT"
                        } else if is_awaiting_review(j) {
                            "AWAIT_REVIEW"
                        } else {
                            "RUNNING"
                        };
                        ListItem::new(Line::from(vec![
                            Span::styled(format!("{} ", marker), Style::default().fg(Color::Cyan)),
                            Span::styled(
                                format!("[{tag}] "),
                                Style::default()
                                    .fg(status_color(j))
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                truncate(&j.name, 30),
                                Style::default().add_modifier(Modifier::BOLD),
                            ),
                            Span::raw("  "),
                            Span::styled(truncate(&j.job_id, 20), Style::default().fg(Color::Gray)),
                        ]))
                    })
                    .collect();
                let jobs_title = if app.focus == Focus::Jobs {
                    "Sessions *"
                } else {
                    "Sessions"
                };
                f.render_widget(
                    List::new(job_items).block(
                        Block::default()
                            .title(jobs_title)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(if app.focus == Focus::Jobs {
                                Color::Cyan
                            } else {
                                Color::DarkGray
                            })),
                    ),
                    top[1],
                );
            }

            let logs_idx = if app.logs_maximized { 2 } else { 3 };
            let footer_idx = if app.logs_maximized { 3 } else { 4 };
            let log_viewport_height = chunks[logs_idx].height.saturating_sub(2) as usize;
            let log_lines = app.log_viewport_lines(log_viewport_height);
            let logs_title = if app.log_scroll == 0 {
                if app.logs_maximized {
                    "Logs (live tail, maximized)".to_string()
                } else {
                    "Logs (live tail)".to_string()
                }
            } else {
                format!("Logs (scrolled {} lines)", app.log_scroll)
            };
            let logs = Paragraph::new(ratatui::text::Text::from(log_lines))
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .title(format!("{logs_title} | legend: agent/output/stderr"))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Blue)),
                );
            f.render_widget(logs, chunks[logs_idx]);

            let footer = Paragraph::new(format!(
                "{} | keys: q quit, tab focus, arrows move, pgup/pgdn logs, home/end logs, m maximize logs, v view, / search, esc clear, r refresh, n new, y approve, c request-changes, a ack, u unack, k kill, t attach(bg), e edit(bg), o open PR",
                app.status
            ))
            .style(Style::default().fg(Color::White).bg(Color::DarkGray).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
            f.render_widget(footer, chunks[footer_idx]);
        })?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(k) = event::read()? {
                if app.search_mode {
                    match k.code {
                        KeyCode::Esc | KeyCode::Enter => {
                            app.search_mode = false;
                            app.ensure_selected_job_visible();
                            app.refresh_logs().await;
                        }
                        KeyCode::Backspace => {
                            app.search.pop();
                            app.ensure_selected_job_visible();
                            app.refresh_logs().await;
                        }
                        KeyCode::Char(c) => {
                            app.search.push(c);
                            app.ensure_selected_job_visible();
                            app.refresh_logs().await;
                        }
                        _ => {}
                    }
                    continue;
                }

                match k.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Tab => {
                        app.focus = if app.focus == Focus::Projects {
                            Focus::Jobs
                        } else {
                            Focus::Projects
                        };
                    }
                    KeyCode::Char('/') => {
                        app.search_mode = true;
                    }
                    KeyCode::Esc => {
                        app.search.clear();
                        app.ensure_selected_job_visible();
                        app.log_scroll = 0;
                        app.refresh_logs().await;
                    }
                    KeyCode::Char('v') => {
                        app.job_view = app.job_view.next();
                        app.ensure_selected_job_visible();
                        app.log_scroll = 0;
                        app.refresh_logs().await;
                        app.status = format!("Session view: {}", app.job_view.label());
                    }
                    KeyCode::Char('m') => {
                        app.logs_maximized = !app.logs_maximized;
                        app.status = if app.logs_maximized {
                            "Logs maximized".to_string()
                        } else {
                            "Logs returned to split layout".to_string()
                        };
                    }
                    KeyCode::Char('d') => {
                        match app.api.discover_project(&app.discover_path).await {
                            Ok(()) => app.status = format!("Discovered {}", app.discover_path),
                            Err(e) => app.status = format!("Discover failed: {e}"),
                        }
                        app.refresh_projects().await;
                        app.refresh_jobs().await;
                    }
                    KeyCode::Char('r') => {
                        app.refresh_projects().await;
                        app.refresh_jobs().await;
                        app.refresh_logs().await;
                        last_refresh = Instant::now();
                    }
                    KeyCode::Char('n') => {
                        if let Some(project) = app.selected_project() {
                            let ts = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            let name = format!("tui-{ts}");
                            let prompt = std::env::var("SHIREI_TUI_PROMPT")
                                .unwrap_or_else(|_| "start an interactive session".to_string());
                            match app.api.spawn_job(&project.project_id, &name, &prompt).await {
                                Ok(()) => app.status = format!("Spawned session {name}"),
                                Err(e) => app.status = format!("Spawn failed: {e}"),
                            }
                            app.refresh_jobs().await;
                            app.log_scroll = 0;
                            app.refresh_logs().await;
                            last_refresh = Instant::now();
                        }
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
                            last_refresh = Instant::now();
                        }
                    }
                    KeyCode::Char('t') => {
                        app.attach_selected_job_background().await;
                    }
                    KeyCode::Char('e') => {
                        app.edit_selected_job_background().await;
                    }
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
                    }
                    KeyCode::PageUp => {
                        let viewport_height = chunks_for_log_height(&terminal, app.logs_maximized)?;
                        app.scroll_logs_up(viewport_height / 2 + 1, viewport_height);
                    }
                    KeyCode::PageDown => {
                        let viewport_height = chunks_for_log_height(&terminal, app.logs_maximized)?;
                        app.scroll_logs_down(viewport_height / 2 + 1);
                    }
                    KeyCode::Home => {
                        let viewport_height = chunks_for_log_height(&terminal, app.logs_maximized)?;
                        app.scroll_logs_up(usize::MAX / 2, viewport_height);
                    }
                    KeyCode::End => {
                        app.log_scroll = 0;
                    }
                    _ => {}
                }
            }
        } else if last_refresh.elapsed() >= refresh_interval {
            app.refresh_projects().await;
            app.refresh_jobs().await;
            app.refresh_logs().await;
            last_refresh = Instant::now();
        }
    }

    Ok(())
}

fn is_waiting_input(job: &JobRow) -> bool {
    if job.status == "RUNNING" && job.needs_attention > 0 {
        return true;
    }
    let s = job.status.to_ascii_uppercase();
    s.contains("WAIT") || s.contains("INPUT")
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

fn chunks_for_log_height<B: ratatui::backend::Backend>(
    terminal: &Terminal<B>,
    logs_maximized: bool,
) -> Result<usize> {
    let size = terminal.size()?;
    let area = Rect::new(0, 0, size.width, size.height);
    let chunks = if logs_maximized {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(3),
            ])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Percentage(55),
                Constraint::Percentage(37),
                Constraint::Length(3),
            ])
            .split(area)
    };
    let logs_idx = if logs_maximized { 2 } else { 3 };
    Ok(chunks[logs_idx].height.saturating_sub(2) as usize)
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
    use super::{fuzzy_match, is_awaiting_review, is_waiting_input, token_candidates, JobRow};

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
            job_id: "a".to_string(),
            name: "n".to_string(),
            status: "RUNNING".to_string(),
            needs_attention: 0,
            origin_repo_key: None,
            origin_pr_number: None,
        };
        let waiting = JobRow {
            job_id: "b".to_string(),
            name: "n".to_string(),
            status: "RUNNING".to_string(),
            needs_attention: 1,
            origin_repo_key: None,
            origin_pr_number: None,
        };
        let review = JobRow {
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
}
