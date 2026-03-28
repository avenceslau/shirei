use anyhow::Result;
use sqlx::Row;
use sqlx::SqlitePool;

use crate::util::now_rfc3339;

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct ProjectRow {
    pub project_id: String,
    pub root_path: String,
    pub git_remote: String,
    pub default_branch: String,
}

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct JobRow {
    pub job_id: String,
    pub project_id: String,
    pub name: String,
    pub status: String,
    pub needs_attention: i64,
    pub runner: String,
    pub cmd: String,
    pub argv_json: String,
    pub cwd: String,
    pub pid: Option<i64>,
    pub log_path: Option<String>,
    pub prompt: Option<String>,
    pub acknowledged_at: Option<String>,
    pub origin_type: String,
    pub origin_repo_key: Option<String>,
    pub origin_pr_number: Option<i64>,
    pub origin_command_id: Option<String>,
    pub worktree_name: Option<String>,
    pub worktree_path: Option<String>,
    pub attach_pid: Option<i64>,
    pub attach_host: Option<String>,
    pub attach_port: Option<i64>,
    pub attach_url: Option<String>,
    pub session_id: Option<String>,
    pub ended_at: Option<String>,
    pub exit_code: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct ActiveSessionRow {
    pub job_id: String,
    pub project_id: String,
    pub name: String,
    pub status: String,
    pub needs_attention: i64,
    pub runner: String,
    pub cwd: String,
    pub worktree_name: Option<String>,
    pub worktree_path: Option<String>,
    pub session_id: Option<String>,
    pub ended_at: Option<String>,
    pub exit_code: Option<i64>,
    pub project_root: String,
}

pub async fn upsert_project(
    pool: &SqlitePool,
    project_id: &str,
    root_path: &str,
    git_remote: &str,
    default_branch: &str,
) -> Result<ProjectRow> {
    let now = now_rfc3339();
    sqlx::query(
        r#"
        INSERT INTO projects (project_id, root_path, git_remote, default_branch, created_at, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ON CONFLICT(root_path) DO UPDATE SET
          git_remote = excluded.git_remote,
          default_branch = excluded.default_branch,
          updated_at = excluded.updated_at
        "#,
    )
    .bind(project_id)
    .bind(root_path)
    .bind(git_remote)
    .bind(default_branch)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    get_project(pool, project_id).await
}

pub async fn get_project(pool: &SqlitePool, project_id: &str) -> Result<ProjectRow> {
    let r = sqlx::query_as::<_, ProjectRow>(
        r#"SELECT project_id, root_path, git_remote, default_branch FROM projects WHERE project_id=?1"#,
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;
    Ok(r)
}

pub async fn list_projects(pool: &SqlitePool) -> Result<Vec<ProjectRow>> {
    Ok(sqlx::query_as::<_, ProjectRow>(
        r#"SELECT project_id, root_path, git_remote, default_branch FROM projects ORDER BY updated_at DESC"#,
    )
    .fetch_all(pool)
    .await?)
}

#[allow(clippy::too_many_arguments)]
pub async fn create_job(
    pool: &SqlitePool,
    project_id: &str,
    name: &str,
    runner: &str,
    cmd: &str,
    argv_json: &str,
    cwd: &str,
    worktree_name: Option<&str>,
    worktree_path: Option<&str>,
) -> Result<JobRow> {
    let job_id = format!("job_{}", uuid::Uuid::new_v4());
    let now = now_rfc3339();

    sqlx::query(
        r#"
        INSERT INTO jobs (job_id, project_id, name, status, needs_attention, runner, cmd, argv_json, cwd,
                          worktree_name, worktree_path,
                          started_at, created_at, updated_at)
        VALUES (?1, ?2, ?3, 'RUNNING', 0, ?4, ?5, ?6, ?7,
                ?8, ?9,
                ?10, ?11, ?12)
        "#,
    )
    .bind(&job_id)
    .bind(project_id)
    .bind(name)
    .bind(runner)
    .bind(cmd)
    .bind(argv_json)
    .bind(cwd)
    .bind(worktree_name)
    .bind(worktree_path)
    .bind(&now)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    get_job(pool, &job_id).await
}

pub async fn get_job(pool: &SqlitePool, job_id: &str) -> Result<JobRow> {
    Ok(sqlx::query_as::<_, JobRow>(
        r#"
        SELECT job_id, project_id, name, status, needs_attention, runner, cmd, argv_json, cwd,
               pid, log_path, prompt, acknowledged_at,
               origin_type, origin_repo_key, origin_pr_number, origin_command_id,
               worktree_name, worktree_path,
               attach_pid, attach_host, attach_port, attach_url,
               session_id, ended_at, exit_code
        FROM jobs WHERE job_id=?1
        "#,
    )
    .bind(job_id)
    .fetch_one(pool)
    .await?)
}

pub async fn list_jobs(pool: &SqlitePool, project_id: &str) -> Result<Vec<JobRow>> {
    Ok(sqlx::query_as::<_, JobRow>(
        r#"
        SELECT job_id, project_id, name, status, needs_attention, runner, cmd, argv_json, cwd,
               pid, log_path, prompt, acknowledged_at,
               origin_type, origin_repo_key, origin_pr_number, origin_command_id,
               worktree_name, worktree_path,
               attach_pid, attach_host, attach_port, attach_url,
               session_id, ended_at, exit_code
        FROM jobs WHERE project_id=?1 ORDER BY updated_at DESC
        "#,
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?)
}

pub async fn latest_job_by_worktree_name(
    pool: &SqlitePool,
    project_id: &str,
    worktree_name: &str,
) -> Result<Option<JobRow>> {
    Ok(sqlx::query_as::<_, JobRow>(
        r#"
        SELECT job_id, project_id, name, status, needs_attention, runner, cmd, argv_json, cwd,
               pid, log_path, prompt, acknowledged_at,
               origin_type, origin_repo_key, origin_pr_number, origin_command_id,
               worktree_name, worktree_path,
               attach_pid, attach_host, attach_port, attach_url,
               session_id, ended_at, exit_code
        FROM jobs
        WHERE project_id=?1 AND worktree_name=?2
        ORDER BY updated_at DESC
        LIMIT 1
        "#,
    )
    .bind(project_id)
    .bind(worktree_name)
    .fetch_optional(pool)
    .await?)
}

pub async fn list_active_sessions(pool: &SqlitePool) -> Result<Vec<ActiveSessionRow>> {
    Ok(sqlx::query_as::<_, ActiveSessionRow>(
        r#"
        SELECT j.job_id, j.project_id, j.name, j.status, j.needs_attention, j.runner,
               j.cwd, j.worktree_name, j.worktree_path, j.session_id,
               j.ended_at, j.exit_code,
               p.root_path AS project_root
        FROM jobs j
        INNER JOIN projects p ON p.project_id = j.project_id
        WHERE (j.worktree_path IS NOT NULL OR j.cwd != '')
          AND j.status IN ('RUNNING', 'AWAITING_REVIEW', 'DONE_UNREAD', 'ERROR')
        ORDER BY j.needs_attention DESC,
                 CASE
                   WHEN j.status = 'AWAITING_REVIEW' THEN 0
                   WHEN j.status = 'RUNNING' THEN 1
                   WHEN j.status = 'DONE_UNREAD' THEN 2
                   ELSE 3
                 END,
                 j.updated_at DESC
        "#,
    )
    .fetch_all(pool)
    .await?)
}

pub async fn job_status(pool: &SqlitePool, job_id: &str) -> Result<Option<String>> {
    Ok(
        sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE job_id=?1")
            .bind(job_id)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn list_running_jobs(pool: &SqlitePool) -> Result<Vec<(String, Option<i64>)>> {
    let rows = sqlx::query(
        "SELECT job_id, pid FROM jobs WHERE status='RUNNING' AND runner != 'interactive' ORDER BY updated_at ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.try_get("job_id").unwrap_or_default(),
                r.try_get("pid").ok().flatten(),
            )
        })
        .collect())
}

pub async fn set_job_session_id(pool: &SqlitePool, job_id: &str, session_id: &str) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query("UPDATE jobs SET session_id=?1, updated_at=?2 WHERE job_id=?3")
        .bind(session_id)
        .bind(&now)
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_job_prompt(pool: &SqlitePool, job_id: &str, prompt: &str) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query("UPDATE jobs SET prompt=?1, updated_at=?2 WHERE job_id=?3")
        .bind(prompt)
        .bind(&now)
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_job_origin_pr(
    pool: &SqlitePool,
    job_id: &str,
    repo_key: &str,
    pr_number: i64,
    command_id: Option<&str>,
) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query(
        r#"UPDATE jobs SET origin_type='pr_watch', origin_repo_key=?1, origin_pr_number=?2, origin_command_id=?3, updated_at=?4 WHERE job_id=?5"#)
        .bind(repo_key)
        .bind(pr_number)
        .bind(command_id)
        .bind(&now)
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn update_job_runtime(
    pool: &SqlitePool,
    job_id: &str,
    pid: Option<i64>,
    log_path: Option<&str>,
) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query("UPDATE jobs SET pid=COALESCE(?1,pid), log_path=COALESCE(?2,log_path), updated_at=?3 WHERE job_id=?4")
        .bind(pid).bind(log_path).bind(&now).bind(job_id).execute(pool).await?;
    Ok(())
}

pub async fn update_job_end(
    pool: &SqlitePool,
    job_id: &str,
    status: &str,
    exit_code: Option<i64>,
) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query(
        "UPDATE jobs SET status=?1, needs_attention=1, ended_at=?2, exit_code=?3, updated_at=?4 WHERE job_id=?5")
        .bind(status).bind(&now).bind(exit_code).bind(&now).bind(job_id).execute(pool).await?;
    Ok(())
}

pub async fn ack_job(pool: &SqlitePool, job_id: &str, ack: bool) -> Result<()> {
    let now = now_rfc3339();
    if ack {
        sqlx::query(
            "UPDATE jobs SET status=CASE WHEN status='DONE_UNREAD' THEN 'DONE_ACKED' WHEN status='AWAITING_REVIEW' THEN 'DONE_APPROVED' ELSE status END, needs_attention=0, acknowledged_at=?1, updated_at=?2 WHERE job_id=?3")
            .bind(&now).bind(&now).bind(job_id).execute(pool).await?;
    } else {
        sqlx::query(
            "UPDATE jobs SET status=CASE WHEN status='DONE_ACKED' THEN 'DONE_UNREAD' WHEN status='DONE_APPROVED' THEN 'AWAITING_REVIEW' ELSE status END, needs_attention=CASE WHEN status IN ('DONE_ACKED','DONE_APPROVED') THEN 1 ELSE needs_attention END, acknowledged_at=NULL, updated_at=?1 WHERE job_id=?2")
            .bind(&now).bind(job_id).execute(pool).await?;
    }
    Ok(())
}

pub async fn approve_job(pool: &SqlitePool, job_id: &str) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query(
        "UPDATE jobs SET status='DONE_APPROVED', needs_attention=0, acknowledged_at=?1, updated_at=?2 WHERE job_id=?3",
    )
    .bind(&now)
    .bind(&now)
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn request_changes(pool: &SqlitePool, job_id: &str) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query(
        "UPDATE jobs SET status='RUNNING', needs_attention=0, acknowledged_at=NULL, ended_at=NULL, exit_code=NULL, updated_at=?1 WHERE job_id=?2",
    )
    .bind(&now)
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_job_attention(
    pool: &SqlitePool,
    job_id: &str,
    needs_attention: bool,
) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query("UPDATE jobs SET needs_attention=?1, updated_at=?2 WHERE job_id=?3")
        .bind(if needs_attention { 1_i64 } else { 0_i64 })
        .bind(&now)
        .bind(job_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_attach_runtime(
    pool: &SqlitePool,
    job_id: &str,
    pid: i64,
    host: &str,
    port: i64,
    url: &str,
) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query(
        "UPDATE jobs SET attach_pid=?1, attach_host=?2, attach_port=?3, attach_url=?4, attach_started_at=?5, updated_at=?6 WHERE job_id=?7",
    )
    .bind(pid)
    .bind(host)
    .bind(port)
    .bind(url)
    .bind(&now)
    .bind(&now)
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn clear_attach_runtime(pool: &SqlitePool, job_id: &str) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query(
        "UPDATE jobs SET attach_pid=NULL, attach_host=NULL, attach_port=NULL, attach_url=NULL, attach_started_at=NULL, updated_at=?1 WHERE job_id=?2",
    )
    .bind(&now)
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn append_job_prompt(pool: &SqlitePool, job_id: &str, extra: &str) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query(
        r#"UPDATE jobs SET prompt=trim(COALESCE(prompt, '') || '

' || ?1), updated_at=?2 WHERE job_id=?3"#,
    )
    .bind(extra)
    .bind(&now)
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn latest_pr_job(
    pool: &SqlitePool,
    repo_key: &str,
    pr_number: i64,
) -> Result<Option<JobRow>> {
    Ok(sqlx::query_as::<_, JobRow>(
        r#"
        SELECT job_id, project_id, name, status, needs_attention, runner, cmd, argv_json, cwd,
               pid, log_path, prompt, acknowledged_at,
               origin_type, origin_repo_key, origin_pr_number, origin_command_id,
               worktree_name, worktree_path,
               attach_pid, attach_host, attach_port, attach_url,
               session_id, ended_at, exit_code
        FROM jobs
        WHERE origin_repo_key=?1 AND origin_pr_number=?2
        ORDER BY updated_at DESC
        LIMIT 1
        "#,
    )
    .bind(repo_key)
    .bind(pr_number)
    .fetch_optional(pool)
    .await?)
}

#[allow(dead_code)]
pub async fn insert_event(
    pool: &SqlitePool,
    project_id: &str,
    subject_id: &str,
    typ: &str,
    payload: &serde_json::Value,
) -> Result<()> {
    let event_id = format!("evt_{}", uuid::Uuid::new_v4());
    let ts = now_rfc3339();
    sqlx::query(
        "INSERT INTO job_events (event_id, project_id, job_id, type, payload_json, ts) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
    )
    .bind(event_id)
    .bind(project_id)
    .bind(subject_id)
    .bind(typ)
    .bind(payload.to_string())
    .bind(ts)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn ensure_watch(
    pool: &SqlitePool,
    project_id: &str,
    repo_key: &str,
    pr_number: i64,
    retention_days: i64,
) -> Result<String> {
    let now = now_rfc3339();
    // If exists, return existing watch_id
    if let Some(watch_id) = sqlx::query_scalar::<_, String>(
        "SELECT watch_id FROM pr_watches WHERE repo_key=?1 AND pr_number=?2",
    )
    .bind(repo_key)
    .bind(pr_number)
    .fetch_optional(pool)
    .await?
    {
        return Ok(watch_id);
    }
    let watch_id = format!("watch_{}", uuid::Uuid::new_v4());
    sqlx::query(
        "INSERT INTO pr_watches (watch_id, project_id, repo_key, pr_number, retention_days, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
    )
    .bind(&watch_id).bind(project_id).bind(repo_key).bind(pr_number).bind(retention_days).bind(&now).bind(&now)
    .execute(pool).await?;
    Ok(watch_id)
}

pub async fn list_watches(
    pool: &SqlitePool,
) -> Result<
    Vec<(
        String,
        String,
        i64,
        i64,
        String,
        Option<String>,
        Option<String>,
    )>,
> {
    let rows = sqlx::query(
        r#"SELECT watch_id, repo_key, pr_number, retention_days, status, merged_at, purge_due_at FROM pr_watches ORDER BY updated_at DESC"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.try_get("watch_id").unwrap_or_default(),
                r.try_get("repo_key").unwrap_or_default(),
                r.try_get("pr_number").unwrap_or_default(),
                r.try_get("retention_days").unwrap_or_default(),
                r.try_get("status").unwrap_or_default(),
                r.try_get("merged_at").ok().flatten(),
                r.try_get("purge_due_at").ok().flatten(),
            )
        })
        .collect())
}

pub async fn stop_watch(pool: &SqlitePool, watch_id: &str) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query("UPDATE pr_watches SET status='STOPPED', updated_at=?1 WHERE watch_id=?2")
        .bind(&now)
        .bind(watch_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_running_watches(
    pool: &SqlitePool,
) -> Result<Vec<(String, String, String, i64, i64)>> {
    let rows = sqlx::query(
        r#"SELECT watch_id, project_id, repo_key, pr_number, last_seen_comment_id FROM pr_watches WHERE status='RUNNING'"#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.try_get("watch_id").unwrap_or_default(),
                r.try_get("project_id").unwrap_or_default(),
                r.try_get("repo_key").unwrap_or_default(),
                r.try_get("pr_number").unwrap_or_default(),
                r.try_get::<Option<i64>, _>("last_seen_comment_id")
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
            )
        })
        .collect())
}

pub async fn update_watch_last_seen(
    pool: &SqlitePool,
    watch_id: &str,
    last_seen: i64,
) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query("UPDATE pr_watches SET last_seen_comment_id=?1, updated_at=?2 WHERE watch_id=?3")
        .bind(last_seen)
        .bind(&now)
        .bind(watch_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn mark_watch_merged(
    pool: &SqlitePool,
    watch_id: &str,
    merged_at: &str,
    purge_due_at: &str,
) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query(
        "UPDATE pr_watches SET status='STOPPED', merged_at=?1, purge_due_at=?2, updated_at=?3 WHERE watch_id=?4"
    ).bind(merged_at).bind(purge_due_at).bind(&now).bind(watch_id).execute(pool).await?;
    Ok(())
}

pub async fn create_pr_command(
    pool: &SqlitePool,
    watch_id: &str,
    source_id: i64,
    author: &str,
    raw: &str,
) -> Result<Option<String>> {
    let now = now_rfc3339();
    // If already exists, skip
    if sqlx::query_scalar::<_, String>(
        "SELECT command_id FROM pr_commands WHERE watch_id=?1 AND source_id=?2",
    )
    .bind(watch_id)
    .bind(source_id)
    .fetch_optional(pool)
    .await?
    .is_some()
    {
        return Ok(None);
    }
    let command_id = format!("cmd_{}", uuid::Uuid::new_v4());
    sqlx::query(
        "INSERT INTO pr_commands (command_id, watch_id, source_id, author_login, raw_text, status, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, 'RECEIVED', ?6, ?7)"
    ).bind(&command_id).bind(watch_id).bind(source_id).bind(author).bind(raw).bind(&now).bind(&now).execute(pool).await?;
    Ok(Some(command_id))
}

pub async fn link_command_job(
    pool: &SqlitePool,
    command_id: &str,
    job_id: &str,
    status: &str,
) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query("UPDATE pr_commands SET job_id=?1, status=?2, updated_at=?3 WHERE command_id=?4")
        .bind(job_id)
        .bind(status)
        .bind(&now)
        .bind(command_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn due_purges(pool: &SqlitePool) -> Result<Vec<(String, String, String, i64)>> {
    let now = now_rfc3339();
    let rows = sqlx::query(
        r#"SELECT watch_id, project_id, repo_key, pr_number FROM pr_watches
           WHERE merged_at IS NOT NULL AND purged_at IS NULL AND purge_due_at IS NOT NULL AND purge_due_at <= ?1"#,
    )
    .bind(now)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.try_get("watch_id").unwrap_or_default(),
                r.try_get("project_id").unwrap_or_default(),
                r.try_get("repo_key").unwrap_or_default(),
                r.try_get("pr_number").unwrap_or_default(),
            )
        })
        .collect())
}

pub async fn mark_watch_purged(pool: &SqlitePool, watch_id: &str) -> Result<()> {
    let now = now_rfc3339();
    sqlx::query("UPDATE pr_watches SET purged_at=?1, updated_at=?2 WHERE watch_id=?3")
        .bind(&now)
        .bind(&now)
        .bind(watch_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn pr_jobs(
    pool: &SqlitePool,
    repo_key: &str,
    pr_number: i64,
) -> Result<Vec<(String, Option<i64>, Option<String>, Option<String>)>> {
    let rows = sqlx::query(
        r#"SELECT job_id, pid, worktree_name, worktree_path
           FROM jobs WHERE origin_type='pr_watch' AND origin_repo_key=?1 AND origin_pr_number=?2"#,
    )
    .bind(repo_key)
    .bind(pr_number)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.try_get("job_id").unwrap_or_default(),
                r.try_get("pid").ok().flatten(),
                r.try_get("worktree_name").ok().flatten(),
                r.try_get("worktree_path").ok().flatten(),
            )
        })
        .collect())
}
