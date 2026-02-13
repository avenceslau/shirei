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
use sqlx::Row;
use sqlx::SqlitePool;
use std::{convert::Infallible, path::PathBuf, sync::Arc};

use crate::{
    config::ShireiConfig, events::EventBus, git_ops, runner::JobRunner, store,
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
        .route("/api/v0.1/jobs/:job_id/logs", get(job_logs))
        .route("/api/v0.1/jobs/:job_id/kill", post(kill_job))
        .route("/api/v0.1/jobs/:job_id/ack", post(ack_job))
        .route("/api/v0.1/jobs/:job_id/unack", post(unack_job))
        .route("/api/v0.1/jobs/:job_id/approve", post(approve_job))
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
    Json(serde_json::json!({
        "state_dir": state.state_dir.display().to_string(),
        "db": state.state_dir.join("shirei.db").display().to_string()
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
            let mut argv = state.config.commands.opencode.args.clone();
            if argv.iter().any(|arg| arg.contains("{job_id}")) {
                argv = argv
                    .into_iter()
                    .map(|arg| arg.replace("{job_id}", &job_id))
                    .collect();
            }

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
        Err(_) => (StatusCode::NOT_FOUND, "JOB_NOT_FOUND").into_response(),
    }
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

    if let Some(feedback) = req.feedback.as_deref() {
        let trimmed = feedback.trim();
        if !trimmed.is_empty() {
            let notes = format!("Reviewer feedback:\n{trimmed}");
            if store::append_job_prompt(&state.pool, &job_id, &notes)
                .await
                .is_err()
            {
                return (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL").into_response();
            }
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
