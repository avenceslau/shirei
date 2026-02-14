use anyhow::Result;
use sqlx::Row;
use sqlx::SqlitePool;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::time::interval;

use crate::{
    config::ShireiConfig,
    events::{EventBus, ShireiEvent},
    git_ops,
    runner::JobRunner,
    store,
    util::{now_rfc3339, request_changes_prompt_notes},
    vcs::{factory::ProviderFactory, parse_repo_key, PullRequestId},
    worktrees,
};

fn parse_command(body: &str) -> Option<String> {
    let s = body.trim();
    if s.starts_with("/shirei ") {
        Some(s.trim_start_matches("/shirei ").trim().to_string())
    } else if s.starts_with("/seirei ") {
        // backward compat
        Some(s.trim_start_matches("/seirei ").trim().to_string())
    } else {
        None
    }
}

pub async fn start(
    pool: SqlitePool,
    bus: EventBus,
    state_dir: PathBuf,
    providers: Arc<ProviderFactory>,
    config: Arc<ShireiConfig>,
) {
    let poll_secs: u64 = std::env::var("SHIREI_PR_POLL_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);
    tracing::info!(poll_interval_secs = poll_secs, "starting PR watch loop");
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_secs(poll_secs));
        loop {
            tick.tick().await;
            if let Err(e) = poll_once(&pool, &bus, &state_dir, &providers, &config).await {
                tracing::error!("pr_watch error: {e:?}");
            }
        }
    });
}

async fn poll_once(
    pool: &SqlitePool,
    bus: &EventBus,
    state_dir: &Path,
    providers: &Arc<ProviderFactory>,
    config: &Arc<ShireiConfig>,
) -> Result<()> {
    let watches = store::list_running_watches(pool).await?;
    tracing::debug!(watch_count = watches.len(), "polling active PR watches");
    for (watch_id, project_id, repo_key, pr_number, last_seen) in watches {
        let Some((kind, repo)) = parse_repo_key(&repo_key) else {
            tracing::warn!(watch_id = %watch_id, repo_key = %repo_key, "skipping watch with invalid repo key");
            continue;
        };
        let provider = providers.get(kind);
        let pr = PullRequestId {
            repo,
            number: pr_number as u64,
        };

        let info = match provider.get_pr(&pr).await {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!("get_pr failed: {e:?}");
                continue;
            }
        };

        // merged?
        if info.merged {
            let merged_at = info.merged_at.unwrap_or_else(now_rfc3339);
            let retention: i64 =
                sqlx::query("SELECT retention_days FROM pr_watches WHERE watch_id=?1")
                    .bind(&watch_id)
                    .fetch_one(pool)
                    .await?
                    .try_get("retention_days")
                    .unwrap_or(7);
            let purge_due = (time::OffsetDateTime::now_utc() + time::Duration::days(retention))
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap();

            store::mark_watch_merged(pool, &watch_id, &merged_at, &purge_due).await?;
            tracing::info!(
                watch_id = %watch_id,
                repo_key = %repo_key,
                pr_number,
                purge_due_at = %purge_due,
                "PR merged; watch marked for purge"
            );
            bus.publish(ShireiEvent{
                event_id: format!("evt_{}", uuid::Uuid::new_v4()),
                ts: now_rfc3339(),
                project_id: project_id.clone(),
                subject_id: format!("prwatch:{watch_id}"),
                r#type: "PR_MERGED".to_string(),
                payload: serde_json::json!({"repo_key": repo_key, "pr_number": pr_number, "purge_due_at": purge_due}),
            });
            // close running jobs (best-effort)
            close_pr_jobs(pool, bus, &project_id, &repo_key, pr_number).await?;
            continue;
        }

        // comments
        let comments = match provider.list_comments(&pr).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("list_comments failed: {e:?}");
                continue;
            }
        };

        let mut max_id = last_seen as u64;
        for c in comments {
            if c.id <= last_seen as u64 {
                continue;
            }
            max_id = max_id.max(c.id);

            let Some(cmd) = parse_command(&c.body) else {
                continue;
            };

            // ensure watch exists already; commands require it; but allow anyway
            let watch_id2 = watch_id.clone();
            if let Some(command_id) =
                store::create_pr_command(pool, &watch_id2, c.id as i64, &c.author, &cmd).await?
            {
                handle_command(
                    pool,
                    bus,
                    state_dir,
                    providers,
                    config,
                    &project_id,
                    &repo_key,
                    pr_number,
                    &command_id,
                    &cmd,
                )
                .await?;
            }
        }

        if max_id as i64 > last_seen {
            store::update_watch_last_seen(pool, &watch_id, max_id as i64).await?;
            tracing::debug!(watch_id = %watch_id, last_seen = max_id as i64, "updated watch cursor");
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_command(
    pool: &SqlitePool,
    bus: &EventBus,
    state_dir: &Path,
    providers: &Arc<ProviderFactory>,
    config: &Arc<ShireiConfig>,
    project_id: &str,
    repo_key: &str,
    pr_number: i64,
    command_id: &str,
    cmd: &str,
) -> Result<()> {
    let (kind, repo) = parse_repo_key(repo_key).ok_or_else(|| anyhow::anyhow!("bad repo_key"))?;
    let provider = providers.get(kind);
    let pr = PullRequestId {
        repo,
        number: pr_number as u64,
    };

    if cmd.starts_with("stop") {
        tracing::info!(project_id = %project_id, repo_key = %repo_key, pr_number, command_id = %command_id, "processing stop command");
        close_pr_jobs(pool, bus, project_id, repo_key, pr_number).await?;
        store::link_command_job(pool, command_id, "", "DONE")
            .await
            .ok();
        let _ = provider
            .create_comment(&pr, "🛑 Shirei: stopping jobs for this PR/MR.")
            .await;
        return Ok(());
    }

    if cmd == "approve" {
        tracing::info!(project_id = %project_id, repo_key = %repo_key, pr_number, command_id = %command_id, "processing approve command");
        if let Some(job) = store::latest_pr_job(pool, repo_key, pr_number).await? {
            store::approve_job(pool, &job.job_id).await?;
            store::link_command_job(pool, command_id, &job.job_id, "DONE").await?;
            let _ = provider
                .create_comment(
                    &pr,
                    &format!("✅ Shirei: approved and closed `{}`.", job.job_id),
                )
                .await;
        } else {
            store::link_command_job(pool, command_id, "", "IGNORED")
                .await
                .ok();
            let _ = provider
                .create_comment(&pr, "⚠️ Shirei: no job found to approve for this PR/MR.")
                .await;
        }
        return Ok(());
    }

    if cmd.starts_with("request-changes") {
        tracing::info!(project_id = %project_id, repo_key = %repo_key, pr_number, command_id = %command_id, "processing request-changes command");
        let feedback = cmd.trim_start_matches("request-changes").trim().to_string();
        if let Some(job) = store::latest_pr_job(pool, repo_key, pr_number).await? {
            if let Some(notes) = request_changes_prompt_notes(Some(&feedback), true) {
                store::append_job_prompt(pool, &job.job_id, &notes).await?;
            }
            store::request_changes(pool, &job.job_id).await?;
            store::link_command_job(pool, command_id, &job.job_id, "RUNNING").await?;

            let runner = JobRunner::new(
                pool.clone(),
                bus.clone(),
                state_dir.to_path_buf(),
                providers.clone(),
                config.clone(),
            );
            let job_id = job.job_id.clone();
            tokio::spawn(async move {
                if let Err(e) = runner.spawn_opencode_job(&job_id).await {
                    tracing::error!(job_id = %job_id, error = ?e, "review rerun failed");
                }
            });

            let _ = provider
                .create_comment(
                    &pr,
                    &format!(
                        "🔁 Shirei: request changes received, rerunning `{}`.",
                        job.job_id
                    ),
                )
                .await;
        } else {
            store::link_command_job(pool, command_id, "", "IGNORED")
                .await
                .ok();
            let _ = provider
                .create_comment(&pr, "⚠️ Shirei: no job found to rerun for this PR/MR.")
                .await;
        }
        return Ok(());
    }

    if cmd.starts_with("respond") {
        tracing::info!(project_id = %project_id, repo_key = %repo_key, pr_number, command_id = %command_id, "processing respond command");
        let _ = provider
            .create_comment(
                &pr,
                "📣 Shirei is watching. Use `/shirei fix <desc>` to spawn a job, then `/shirei approve` or `/shirei request-changes <feedback>`.",
            )
            .await;
        store::link_command_job(pool, command_id, "", "DONE")
            .await
            .ok();
        return Ok(());
    }

    if cmd.starts_with("fix ") {
        let desc = cmd.trim_start_matches("fix ").trim();
        tracing::info!(project_id = %project_id, repo_key = %repo_key, pr_number, command_id = %command_id, "processing fix command");
        // project repo
        let proj = store::get_project(pool, project_id).await?;
        let repo = git_ops::open_repo(std::path::Path::new(&proj.root_path))?;

        // fetch + get PR head sha
        let info = provider
            .get_pr(&pr)
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let head_sha = info
            .head_sha
            .ok_or_else(|| anyhow::anyhow!("missing head_sha"))?;

        // Fetch origin. NOTE: fork PRs may require extra fetch logic (future enhancement).
        let _ = git_ops::fetch_origin(&repo);

        let branch = format!(
            "shirei/pr-{}-{}",
            pr_number,
            &command_id[4..12.min(command_id.len())]
        );
        let _ = git_ops::create_or_update_branch_at_sha(&repo, &branch, &head_sha);

        // create worktree
        let wt_root = worktrees::root(state_dir, project_id);
        tokio::fs::create_dir_all(&wt_root).await.ok();
        let worktree_name = format!(
            "pr-{}-{}",
            pr_number,
            &command_id[4..12.min(command_id.len())]
        );
        let worktree_path = worktrees::alloc(&wt_root, &worktree_name)?;

        let base_ref = format!("refs/heads/{branch}");
        let _ = git_ops::create_worktree(
            &repo,
            &worktree_name,
            &worktree_path,
            &base_ref,
            Some(&branch),
        );

        // create job row
        let job = store::create_job(
            pool,
            project_id,
            &format!("pr{pr_number}-{worktree_name}"),
            "opencode",
            "opencode",
            "[]",
            &worktree_path.display().to_string(),
            Some(&worktree_name),
            Some(&worktree_path.display().to_string()),
        )
        .await?;

        store::set_job_prompt(pool, &job.job_id, desc).await?;
        store::set_job_origin_pr(pool, &job.job_id, repo_key, pr_number, Some(command_id)).await?;
        store::link_command_job(pool, command_id, &job.job_id, "RUNNING").await?;

        tracing::info!(
            project_id = %project_id,
            repo_key = %repo_key,
            pr_number,
            command_id = %command_id,
            job_id = %job.job_id,
            worktree = %worktree_path.display(),
            "spawned PR fix job"
        );

        let _ = provider
            .create_comment(
                &pr,
                &format!(
                    "✅ Shirei received: `{}`\nSpawning job `{}`",
                    cmd, job.job_id
                ),
            )
            .await;

        // spawn runner in background
        let runner = JobRunner::new(
            pool.clone(),
            bus.clone(),
            state_dir.to_path_buf(),
            providers.clone(),
            config.clone(),
        );
        let job_id = job.job_id.clone();
        tokio::spawn(async move {
            if let Err(e) = runner.spawn_opencode_job(&job_id).await {
                tracing::error!(job_id = %job_id, error = ?e, "PR job runner failed");
            }
        });

        bus.publish(ShireiEvent{
            event_id: format!("evt_{}", uuid::Uuid::new_v4()),
            ts: now_rfc3339(),
            project_id: project_id.to_string(),
            subject_id: job.job_id.clone(),
            r#type: "PR_JOB_STARTED".to_string(),
            payload: serde_json::json!({"repo_key": repo_key, "pr_number": pr_number, "command_id": command_id}),
        });

        return Ok(());
    }

    // Unknown command
    tracing::info!(project_id = %project_id, repo_key = %repo_key, pr_number, command_id = %command_id, command = %cmd, "ignoring unknown PR command");
    store::link_command_job(pool, command_id, "", "IGNORED")
        .await
        .ok();
    let _ = provider
        .create_comment(
            &pr,
            "⚠️ Shirei: unknown command. Try `/shirei fix <desc>`, `/shirei approve`, `/shirei request-changes <feedback>`, or `/shirei stop`.",
        )
        .await;
    Ok(())
}

async fn close_pr_jobs(
    pool: &SqlitePool,
    bus: &EventBus,
    project_id: &str,
    repo_key: &str,
    pr_number: i64,
) -> Result<()> {
    let jobs = store::pr_jobs(pool, repo_key, pr_number).await?;
    tracing::info!(
        project_id = %project_id,
        repo_key = %repo_key,
        pr_number,
        jobs = jobs.len(),
        "closing PR jobs"
    );
    for (job_id, pid, _wt, _path) in jobs {
        if let Some(pid) = pid {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
            tracing::debug!(job_id = %job_id, pid, "sent SIGTERM to job process group");
        }
        bus.publish(ShireiEvent {
            event_id: format!("evt_{}", uuid::Uuid::new_v4()),
            ts: now_rfc3339(),
            project_id: project_id.to_string(),
            subject_id: job_id,
            r#type: "JOB_CLOSED".to_string(),
            payload: serde_json::json!({"repo_key": repo_key, "pr_number": pr_number}),
        });
    }
    Ok(())
}
