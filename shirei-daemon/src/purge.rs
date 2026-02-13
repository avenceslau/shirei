use anyhow::Result;
use sqlx::SqlitePool;
use std::{path::PathBuf, time::Duration};
use tokio::time::interval;

use crate::{
    events::{EventBus, ShireiEvent},
    git_ops, store,
    util::now_rfc3339,
};

pub async fn start(pool: SqlitePool, bus: EventBus, state_dir: PathBuf) {
    tracing::info!(interval = "hourly", "starting purge loop");
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_secs(60 * 60)); // hourly
        loop {
            tick.tick().await;
            if let Err(e) = run(&pool, &bus, &state_dir).await {
                tracing::error!("purge error: {e:?}");
            }
        }
    });
}

async fn run(pool: &SqlitePool, bus: &EventBus, _state_dir: &PathBuf) -> Result<()> {
    let due = store::due_purges(pool).await?;
    tracing::debug!(due_watches = due.len(), "running purge pass");
    for (watch_id, project_id, repo_key, pr_number) in due {
        tracing::info!(watch_id = %watch_id, repo_key = %repo_key, pr_number, "purging merged PR resources");
        let proj = store::get_project(pool, &project_id).await?;
        let main_repo = git_ops::open_repo(std::path::Path::new(&proj.root_path))?;

        let jobs = store::pr_jobs(pool, &repo_key, pr_number).await?;
        for (_job_id, _pid, wt_name, wt_path) in jobs {
            if let (Some(wt_name), Some(wt_path)) = (wt_name, wt_path) {
                // safety: skip dirty worktrees
                if let Ok(wtrepo) = git_ops::open_repo(std::path::Path::new(&wt_path)) {
                    if git_ops::is_dirty(&wtrepo)? {
                        tracing::warn!(watch_id = %watch_id, worktree = %wt_path, "skipping dirty worktree during purge");
                        bus.publish(ShireiEvent {
                            event_id: format!("evt_{}", uuid::Uuid::new_v4()),
                            ts: now_rfc3339(),
                            project_id: project_id.clone(),
                            subject_id: format!("prwatch:{watch_id}"),
                            r#type: "PR_PURGE_BLOCKED".to_string(),
                            payload: serde_json::json!({"worktree": wt_path}),
                        });
                        continue;
                    }
                }

                let _ = git_ops::prune_worktree(&main_repo, &wt_name);
                let _ = tokio::fs::remove_dir_all(&wt_path).await;
                tracing::info!(watch_id = %watch_id, worktree = %wt_path, "purged PR worktree");
            }
        }

        store::mark_watch_purged(pool, &watch_id).await?;
        tracing::info!(watch_id = %watch_id, "marked watch as purged");

        bus.publish(ShireiEvent {
            event_id: format!("evt_{}", uuid::Uuid::new_v4()),
            ts: now_rfc3339(),
            project_id: project_id.clone(),
            subject_id: format!("prwatch:{watch_id}"),
            r#type: "PR_PURGED".to_string(),
            payload: serde_json::json!({"repo_key": repo_key, "pr_number": pr_number}),
        });
    }
    Ok(())
}
