use anyhow::{anyhow, Context};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use opencode_sdk::types::permission::{PermissionReply, PermissionReplyRequest};
use opencode_sdk::types::question::QuestionReplyRequest;
use opencode_sdk::{Client as OpencodeClient, ClientBuilder as OpencodeClientBuilder};
use sqlx::Row;
use sqlx::SqlitePool;
use std::{convert::Infallible, path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use crate::{
    config::ShireiConfig, events::EventBus, git_ops, runner::JobRunner, store, util,
    vcs::factory::ProviderFactory, worktrees,
};

#[derive(Clone)]
pub struct AppState {
    pub state_dir: PathBuf,
    pub auth_token: String,
    pub pool: SqlitePool,
    pub bus: EventBus,
    pub providers: Arc<ProviderFactory>,
    pub config: Arc<ShireiConfig>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v0.1/health", get(health))
        .route("/api/v0.1/info", get(info))
        .route("/api/v0.1/projects", get(list_projects))
        .route("/api/v0.1/projects/discover", post(discover_project))
        .route(
            "/api/v0.1/projects/:project_id/jobs",
            get(list_jobs).post(create_job),
        )
        .route("/api/v0.1/jobs/:job_id", get(get_job))
        .route("/api/v0.1/jobs/:job_id/attach", post(job_attach))
        .route(
            "/api/v0.1/jobs/:job_id/attach-status",
            get(job_attach_status),
        )
        .route("/api/v0.1/jobs/:job_id/attach-stop", post(job_attach_stop))
        .route("/api/v0.1/jobs/:job_id/logs", get(job_logs))
        .route("/api/v0.1/jobs/:job_id/kill", post(kill_job))
        .route("/api/v0.1/jobs/:job_id/ack", post(ack_job))
        .route("/api/v0.1/jobs/:job_id/unack", post(unack_job))
        .route("/api/v0.1/jobs/:job_id/approve", post(approve_job))
        .route("/api/v0.1/jobs/:job_id/inputs", get(job_inputs))
        .route(
            "/api/v0.1/jobs/:job_id/permissions/:request_id/reply",
            post(job_permission_reply),
        )
        .route(
            "/api/v0.1/jobs/:job_id/questions/:request_id/reply",
            post(job_question_reply),
        )
        .route(
            "/api/v0.1/jobs/:job_id/questions/:request_id/reject",
            post(job_question_reject),
        )
        .route(
            "/api/v0.1/jobs/:job_id/request-changes",
            post(request_changes_job),
        )
        .route("/api/v0.1/watch-pr", post(watch_start).get(watch_list))
        .route("/api/v0.1/watch-pr/:watch_id/stop", post(watch_stop))
        .route("/api/v0.1/events/stream", get(events_stream))
        .with_state(state)
}

fn require_auth(headers: &HeaderMap, token: &str) -> Result<(), StatusCode> {
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = format!("Bearer {}", token);
    if auth == expected {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status":"ok","api_version":"v0.1"}))
}

async fn info(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    let opencode_program = state.config.commands.opencode.program.clone();
    let opencode_args = state.config.commands.opencode.args.clone();
    Json(serde_json::json!({
        "state_dir": state.state_dir.display().to_string(),
        "db": state.state_dir.join("shirei.db").display().to_string(),
        "providers": {
            "github_configured": std::env::var("SHIREI_GITHUB_TOKEN").ok().map(|v| !v.trim().is_empty()).unwrap_or(false),
            "gitlab_configured": std::env::var("SHIREI_GITLAB_TOKEN").ok().map(|v| !v.trim().is_empty()).unwrap_or(false)
                && std::env::var("SHIREI_GITLAB_HOST").ok().map(|v| !v.trim().is_empty()).unwrap_or(false)
        },
        "commands": {
            "opencode_program": opencode_program,
            "opencode_args": opencode_args,
        }
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct DiscoverReq {
    root_path: String,
}

async fn discover_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<DiscoverReq>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }

    let root = crate::canonicalize_lossy(&req.root_path);
    if !std::path::Path::new(&root).exists() {
        return (StatusCode::BAD_REQUEST, "PATH_NOT_FOUND").into_response();
    }

    let git_remote = crate::git_remote(&root).await.unwrap_or_default();
    let default_branch = crate::default_branch(&root)
        .await
        .unwrap_or_else(|| "main".to_string());

    let ns = uuid::Uuid::NAMESPACE_URL;
    let material = format!("{}|{}", root, git_remote);
    let id = uuid::Uuid::new_v5(&ns, material.as_bytes());
    let project_id = format!("proj_{}", id);

    match store::upsert_project(
        &state.pool,
        &project_id,
        &root,
        &git_remote,
        &default_branch,
    )
    .await
    {
        Ok(p) => Json(serde_json::json!({"project": p})).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL").into_response(),
    }
}

async fn list_projects(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    match store::list_projects(&state.pool).await {
        Ok(ps) => Json(serde_json::json!({"projects": ps})).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL").into_response(),
    }
}

async fn list_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    match store::list_jobs(&state.pool, &project_id).await {
        Ok(js) => Json(serde_json::json!({"jobs": js})).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL").into_response(),
    }
}

async fn get_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    match store::get_job(&state.pool, &job_id).await {
        Ok(job) => Json(serde_json::json!({"job": job})).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "JOB_NOT_FOUND").into_response(),
    }
}

async fn job_attach(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }

    match store::get_job(&state.pool, &job_id).await {
        Ok(job) => {
            if std::env::var("SHIREI_OPENCODE_CMD").is_ok() {
                let mut argv = state.config.commands.opencode.args.clone();
                if argv.iter().any(|arg| arg.contains("{job_id}")) {
                    argv = argv
                        .into_iter()
                        .map(|arg| arg.replace("{job_id}", &job_id))
                        .collect();
                }

                return Json(serde_json::json!({
                    "attach": {
                        "job_id": job_id,
                        "status": job.status,
                        "cwd": job.cwd,
                        "cmd": state.config.commands.opencode.program.clone(),
                        "argv": argv,
                    }
                }))
                .into_response();
            }

            match ensure_job_attach_runtime(&state, &job).await {
                Ok(attach_url) => {
                    let mut argv = state.config.commands.opencode.args.clone();
                    argv.push("attach".to_string());
                    argv.push(attach_url);

                    Json(serde_json::json!({
                        "attach": {
                            "job_id": job_id,
                            "status": job.status,
                            "cwd": job.cwd,
                            "cmd": state.config.commands.opencode.program.clone(),
                            "argv": argv,
                        }
                    }))
                    .into_response()
                }
                Err(e) => {
                    tracing::error!(job_id = %job_id, error = ?e, "ensure attach runtime failed");
                    (StatusCode::BAD_GATEWAY, "ATTACH_RUNTIME_FAILED").into_response()
                }
            }
        }
        Err(_) => (StatusCode::NOT_FOUND, "JOB_NOT_FOUND").into_response(),
    }
}

async fn job_attach_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    match store::get_job(&state.pool, &job_id).await {
        Ok(job) => {
            let running = match (job.attach_pid, job.attach_host.as_deref(), job.attach_port) {
                (Some(pid), Some(_), Some(_)) if process_alive(pid) => true,
                _ => false,
            };
            Json(serde_json::json!({
                "job_id": job_id,
                "running": running,
                "attach_url": job.attach_url,
                "attach_pid": job.attach_pid,
                "attach_host": job.attach_host,
                "attach_port": job.attach_port,
            }))
            .into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "JOB_NOT_FOUND").into_response(),
    }
}

async fn job_attach_stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    match stop_job_attach_runtime(&state, &job_id).await {
        Ok(stopped) => Json(serde_json::json!({"ok": true, "stopped": stopped})).into_response(),
        Err(e) => {
            tracing::error!(job_id = %job_id, error = ?e, "attach-stop failed");
            (StatusCode::BAD_GATEWAY, "ATTACH_STOP_FAILED").into_response()
        }
    }
}

async fn ensure_job_attach_runtime(
    state: &AppState,
    job: &store::JobRow,
) -> anyhow::Result<String> {
    if let (Some(pid), Some(url)) = (job.attach_pid, job.attach_url.clone()) {
        if process_alive(pid) && attach_health_ok(&url).await {
            return Ok(url);
        }
        let _ = stop_pid(pid);
        let _ = store::clear_attach_runtime(&state.pool, &job.job_id).await;
    }

    let host = "127.0.0.1";
    let listener = std::net::TcpListener::bind((host, 0)).context("bind ephemeral attach port")?;
    let port = listener
        .local_addr()
        .map(|a| a.port())
        .context("read attach port")?;
    drop(listener);

    let mut cmd = std::process::Command::new(&state.config.commands.opencode.program);
    cmd.args(&state.config.commands.opencode.args)
        .arg("serve")
        .arg("--hostname")
        .arg(host)
        .arg("--port")
        .arg(port.to_string())
        .current_dir(&job.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let child = cmd.spawn().context("spawn opencode serve for attach")?;
    let pid = child.id() as i64;
    let url = format!("http://{host}:{port}");

    wait_for_attach_health(&url)
        .await
        .with_context(|| format!("attach backend did not become healthy at {url}"))?;

    store::set_attach_runtime(&state.pool, &job.job_id, pid, host, port as i64, &url).await?;
    Ok(url)
}

async fn stop_job_attach_runtime(state: &AppState, job_id: &str) -> anyhow::Result<bool> {
    let job = store::get_job(&state.pool, job_id).await?;
    if let Some(pid) = job.attach_pid {
        let _ = stop_pid(pid);
        store::clear_attach_runtime(&state.pool, job_id).await?;
        return Ok(true);
    }
    Ok(false)
}

async fn wait_for_attach_health(url: &str) -> anyhow::Result<()> {
    for _ in 0..50 {
        if attach_health_ok(url).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!("attach backend health timeout"))
}

async fn attach_health_ok(url: &str) -> bool {
    let client = reqwest::Client::new();
    match client.get(format!("{url}/global/health")).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

#[cfg(unix)]
fn process_alive(pid: i64) -> bool {
    let rc = unsafe { libc::kill(pid as i32, 0) };
    if rc == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error().raw_os_error();
    !matches!(err, Some(libc::ESRCH))
}

#[cfg(not(unix))]
fn process_alive(_pid: i64) -> bool {
    false
}

#[cfg(unix)]
fn stop_pid(pid: i64) -> bool {
    unsafe {
        let _ = libc::kill(pid as i32, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_millis(250));
    if !process_alive(pid) {
        return true;
    }
    unsafe {
        let _ = libc::kill(pid as i32, libc::SIGKILL);
    }
    !process_alive(pid)
}

#[cfg(not(unix))]
fn stop_pid(_pid: i64) -> bool {
    false
}

#[derive(serde::Deserialize)]
struct CreateJobReq {
    name: String,
    prompt: Option<String>,
}

async fn create_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_id): Path<String>,
    Json(req): Json<CreateJobReq>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }

    let proj = match store::get_project(&state.pool, &project_id).await {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "PROJECT_NOT_FOUND").into_response(),
    };

    tracing::info!(
        project_id = %project_id,
        job_name = %req.name,
        has_prompt = req.prompt.is_some(),
        "creating manual job"
    );

    let repo = match git_ops::open_repo(std::path::Path::new(&proj.root_path)) {
        Ok(r) => r,
        Err(_) => return (StatusCode::BAD_REQUEST, "PROJECT_NOT_GIT_REPO").into_response(),
    };
    let base_ref = resolve_manual_base_ref(&repo, &proj.default_branch);
    let Some(base_ref) = base_ref else {
        return (StatusCode::BAD_REQUEST, "PROJECT_BASE_REF_NOT_FOUND").into_response();
    };

    let worktree_root = worktrees::root(&state.state_dir, &project_id);
    let _ = tokio::fs::create_dir_all(&worktree_root).await;

    let suffix = uuid::Uuid::new_v4().to_string();
    let short = &suffix[..8];
    let worktree_name = format!("job-{}-{}", sanitize_name(&req.name), short);
    let branch = format!("shirei/{worktree_name}");
    let worktree_path = match worktrees::alloc(&worktree_root, &worktree_name) {
        Ok(p) => p,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "WORKTREE_ALLOC_FAILED").into_response()
        }
    };
    if git_ops::create_worktree(
        &repo,
        &worktree_name,
        &worktree_path,
        &base_ref,
        Some(&branch),
    )
    .is_err()
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, "WORKTREE_CREATE_FAILED").into_response();
    }

    let job = match store::create_job(
        &state.pool,
        &project_id,
        &req.name,
        "opencode",
        "opencode",
        "[]",
        &worktree_path.display().to_string(),
        Some(&worktree_name),
        Some(&worktree_path.display().to_string()),
    )
    .await
    {
        Ok(j) => j,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "JOB_CREATE_FAILED").into_response(),
    };

    if let Some(p) = req.prompt.as_deref() {
        let _ = store::set_job_prompt(&state.pool, &job.job_id, p).await;
    }

    tracing::info!(
        project_id = %project_id,
        job_id = %job.job_id,
        cwd = %job.cwd,
        worktree = %worktree_path.display(),
        "job created; scheduling runner"
    );

    let runner = JobRunner::new(
        state.pool.clone(),
        state.bus.clone(),
        state.state_dir.clone(),
        state.providers.clone(),
        state.config.clone(),
    );
    let job_id = job.job_id.clone();
    tokio::spawn(async move {
        if let Err(e) = runner.spawn_opencode_job(&job_id).await {
            tracing::error!(job_id = %job_id, error = ?e, "manual job runner failed");
        }
    });

    Json(serde_json::json!({"job": job})).into_response()
}

fn sanitize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "session".to_string()
    } else {
        trimmed.chars().take(24).collect()
    }
}

fn resolve_manual_base_ref(repo: &git2::Repository, default_branch: &str) -> Option<String> {
    let local = format!("refs/heads/{default_branch}");
    if repo.find_reference(&local).is_ok() {
        return Some(local);
    }
    let remote = format!("refs/remotes/origin/{default_branch}");
    if repo.find_reference(&remote).is_ok() {
        return Some(remote);
    }
    if repo.find_reference("refs/heads/main").is_ok() {
        return Some("refs/heads/main".to_string());
    }
    if repo.find_reference("refs/heads/master").is_ok() {
        return Some("refs/heads/master".to_string());
    }
    if repo.head().is_ok() {
        return Some("HEAD".to_string());
    }
    None
}

#[derive(serde::Deserialize)]
struct LogsQ {
    tail_lines: Option<usize>,
}

async fn job_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Query(q): Query<LogsQ>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    let rec = sqlx::query("SELECT log_path FROM jobs WHERE job_id=?1")
        .bind(&job_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
    let Some(r) = rec else {
        return (StatusCode::NOT_FOUND, "JOB_NOT_FOUND").into_response();
    };
    let path: Option<String> = r.try_get("log_path").ok().flatten();
    let Some(path) = path else {
        return Json(serde_json::json!({"lines": []})).into_response();
    };
    let n = q.tail_lines.unwrap_or(200).min(5000);
    let data = tokio::fs::read_to_string(&path).await.unwrap_or_default();
    let mut lines: Vec<&str> = data.lines().collect();
    if lines.len() > n {
        lines = lines[lines.len() - n..].to_vec();
    }
    Json(serde_json::json!({"job_id": job_id, "lines": lines})).into_response()
}

async fn kill_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    let _ = stop_job_attach_runtime(&state, &job_id).await;

    let pid = sqlx::query("SELECT pid FROM jobs WHERE job_id=?1")
        .bind(&job_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .and_then(|r| r.try_get::<Option<i64>, _>("pid").ok().flatten());
    let Some(pid) = pid else {
        return (StatusCode::BAD_REQUEST, "NO_PID").into_response();
    };

    tracing::info!(job_id = %job_id, pid, "terminating job process group");
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    let _ = store::update_job_end(&state.pool, &job_id, "ERROR", None).await;
    Json(serde_json::json!({"killed": true, "pid": pid})).into_response()
}

async fn ack_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    let _ = store::ack_job(&state.pool, &job_id, true).await;
    Json(serde_json::json!({"ok": true})).into_response()
}
async fn unack_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    let _ = store::ack_job(&state.pool, &job_id, false).await;
    Json(serde_json::json!({"ok": true})).into_response()
}

async fn approve_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    let _ = stop_job_attach_runtime(&state, &job_id).await;
    match store::approve_job(&state.pool, &job_id).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL").into_response(),
    }
}

#[derive(serde::Deserialize)]
struct RequestChangesReq {
    feedback: Option<String>,
}

async fn request_changes_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Json(req): Json<RequestChangesReq>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }

    let job = match store::get_job(&state.pool, &job_id).await {
        Ok(job) => job,
        Err(_) => return (StatusCode::NOT_FOUND, "JOB_NOT_FOUND").into_response(),
    };

    let require_pr_push = job.origin_type == "pr_watch";
    if let Some(notes) =
        util::request_changes_prompt_notes(req.feedback.as_deref(), require_pr_push)
    {
        if store::append_job_prompt(&state.pool, &job_id, &notes)
            .await
            .is_err()
        {
            return (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL").into_response();
        }
    }

    if store::request_changes(&state.pool, &job_id).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL").into_response();
    }

    let runner = JobRunner::new(
        state.pool.clone(),
        state.bus.clone(),
        state.state_dir.clone(),
        state.providers.clone(),
        state.config.clone(),
    );
    tokio::spawn(async move {
        if let Err(e) = runner.spawn_opencode_job(&job_id).await {
            tracing::error!(job_id = %job_id, error = ?e, "request-changes rerun failed");
        }
    });

    Json(serde_json::json!({"ok": true, "job_id": job.job_id})).into_response()
}

fn opencode_client_for_dir(dir: &str) -> Result<OpencodeClient, String> {
    let base_url = std::env::var("SHIREI_OPENCODE_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:4096".to_string());
    OpencodeClientBuilder::new()
        .base_url(base_url)
        .directory(dir.to_string())
        .build()
        .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
struct PermissionPrompt {
    id: String,
    session_id: Option<String>,
    permission: String,
    patterns: Vec<String>,
}

#[derive(serde::Serialize)]
struct QuestionPrompt {
    id: String,
    session_id: String,
    header: String,
    question: String,
    options: Vec<String>,
    multiple: bool,
    custom: bool,
}

async fn job_inputs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }

    let job = match store::get_job(&state.pool, &job_id).await {
        Ok(job) => job,
        Err(_) => return (StatusCode::NOT_FOUND, "JOB_NOT_FOUND").into_response(),
    };

    let client = match opencode_client_for_dir(&job.cwd) {
        Ok(client) => client,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "OPENCODE_CLIENT_FAILED").into_response()
        }
    };

    let permissions = client.permissions().list().await.unwrap_or_default();
    let questions = client.questions().list().await.unwrap_or_default();

    let p: Vec<PermissionPrompt> = permissions
        .into_iter()
        .map(|it| PermissionPrompt {
            id: it.id,
            session_id: it.session_id,
            permission: it.permission,
            patterns: it.patterns,
        })
        .collect();

    let q: Vec<QuestionPrompt> = questions
        .into_iter()
        .flat_map(|it| {
            let sid = it.session_id;
            it.questions.into_iter().map(move |qq| QuestionPrompt {
                id: it.id.clone(),
                session_id: sid.clone(),
                header: qq.header,
                question: qq.question,
                options: qq.options.into_iter().map(|o| o.label).collect(),
                multiple: qq.multiple.unwrap_or(false),
                custom: qq.custom.unwrap_or(false),
            })
        })
        .collect();

    Json(serde_json::json!({
        "job_id": job_id,
        "permissions": p,
        "questions": q,
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct PermissionReplyReq {
    reply: String,
    message: Option<String>,
}

async fn job_permission_reply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((job_id, request_id)): Path<(String, String)>,
    Json(req): Json<PermissionReplyReq>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    let job = match store::get_job(&state.pool, &job_id).await {
        Ok(job) => job,
        Err(_) => return (StatusCode::NOT_FOUND, "JOB_NOT_FOUND").into_response(),
    };
    let client = match opencode_client_for_dir(&job.cwd) {
        Ok(client) => client,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "OPENCODE_CLIENT_FAILED").into_response()
        }
    };

    let reply = match req.reply.to_ascii_lowercase().as_str() {
        "once" => PermissionReply::Once,
        "always" => PermissionReply::Always,
        "reject" => PermissionReply::Reject,
        _ => return (StatusCode::BAD_REQUEST, "INVALID_REPLY").into_response(),
    };
    let body = PermissionReplyRequest {
        reply,
        message: req.message,
    };
    match client.permissions().reply(&request_id, &body).await {
        Ok(_) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(_) => (StatusCode::BAD_GATEWAY, "OPENCODE_REPLY_FAILED").into_response(),
    }
}

#[derive(serde::Deserialize)]
struct QuestionReplyReq {
    answers: Vec<Vec<String>>,
}

async fn job_question_reply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((job_id, request_id)): Path<(String, String)>,
    Json(req): Json<QuestionReplyReq>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    let job = match store::get_job(&state.pool, &job_id).await {
        Ok(job) => job,
        Err(_) => return (StatusCode::NOT_FOUND, "JOB_NOT_FOUND").into_response(),
    };
    let client = match opencode_client_for_dir(&job.cwd) {
        Ok(client) => client,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "OPENCODE_CLIENT_FAILED").into_response()
        }
    };
    let body = QuestionReplyRequest {
        answers: req.answers,
    };
    match client.questions().reply(&request_id, &body).await {
        Ok(ok) => Json(serde_json::json!({"ok": ok})).into_response(),
        Err(_) => (StatusCode::BAD_GATEWAY, "OPENCODE_REPLY_FAILED").into_response(),
    }
}

async fn job_question_reject(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((job_id, request_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    let job = match store::get_job(&state.pool, &job_id).await {
        Ok(job) => job,
        Err(_) => return (StatusCode::NOT_FOUND, "JOB_NOT_FOUND").into_response(),
    };
    let client = match opencode_client_for_dir(&job.cwd) {
        Ok(client) => client,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "OPENCODE_CLIENT_FAILED").into_response()
        }
    };
    match client.questions().reject(&request_id).await {
        Ok(ok) => Json(serde_json::json!({"ok": ok})).into_response(),
        Err(_) => (StatusCode::BAD_GATEWAY, "OPENCODE_REPLY_FAILED").into_response(),
    }
}

#[derive(serde::Deserialize)]
struct WatchStartReq {
    project_id: String,
    repo_key: String,
    pr_number: i64,
    retention_days: Option<i64>,
}

async fn watch_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<WatchStartReq>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    let retention = req.retention_days.unwrap_or(7);
    tracing::info!(
        project_id = %req.project_id,
        repo_key = %req.repo_key,
        pr_number = req.pr_number,
        retention_days = retention,
        "registering PR watch"
    );
    match store::ensure_watch(
        &state.pool,
        &req.project_id,
        &req.repo_key,
        req.pr_number,
        retention,
    )
    .await
    {
        Ok(watch_id) => Json(serde_json::json!({"watch_id": watch_id})).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL").into_response(),
    }
}

async fn watch_list(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    match store::list_watches(&state.pool).await {
        Ok(ws) => Json(serde_json::json!({"watches": ws})).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL").into_response(),
    }
}

async fn watch_stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(watch_id): Path<String>,
) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    tracing::info!(watch_id = %watch_id, "stopping PR watch");
    let _ = store::stop_watch(&state.pool, &watch_id).await;
    Json(serde_json::json!({"ok": true})).into_response()
}

async fn events_stream(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(code) = require_auth(&headers, &state.auth_token) {
        return code.into_response();
    }
    let mut rx = state.bus.subscribe();
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(evt) => {
                    let json = serde_json::to_string(&evt).unwrap_or_else(|_| "{}".to_string());
                    yield Ok::<Event, Infallible>(Event::default().event("shirei").data(json));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    };
    Sse::new(stream).into_response()
}
