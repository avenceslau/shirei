mod api;
mod config;
mod db;
mod events;
mod git_ops;
mod pr_watch;
mod purge;
mod runner;
mod store;
mod util;
mod vcs;
mod worktrees;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use api::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "shirei_daemon=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let state_dir = std::env::var("SHIREI_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".local/share/shirei"));
    std::fs::create_dir_all(&state_dir)?;
    let config = Arc::new(config::ShireiConfig::load(&state_dir));

    // token
    let token_path = state_dir.join("token");
    let auth_token = if token_path.exists() {
        std::fs::read_to_string(&token_path)?.trim().to_string()
    } else {
        let t = format!("shirei_{}", uuid::Uuid::new_v4());
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&token_path)?;
            writeln!(f, "{t}")?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&token_path, format!("{t}\n"))?;
        }
        t
    };

    let db = db::Db::new(state_dir.clone()).await?;

    let bus = events::EventBus::new(2048);
    let providers = Arc::new(vcs::factory::ProviderFactory::from_env().await?);

    // background loops
    pr_watch::start(
        db.pool.clone(),
        bus.clone(),
        state_dir.clone(),
        providers.clone(),
        config.clone(),
    )
    .await;
    purge::start(db.pool.clone(), bus.clone(), state_dir.clone()).await;
    runner::recover_running_jobs(
        db.pool.clone(),
        bus.clone(),
        state_dir.clone(),
        providers.clone(),
        config.clone(),
    )
    .await?;

    let state = AppState {
        state_dir: state_dir.clone(),
        auth_token,
        pool: db.pool.clone(),
        bus,
        providers,
        config,
    };

    let app = api::router(state).layer(TraceLayer::new_for_http());

    let addr: SocketAddr = std::env::var("SHIREI_BIND")
        .unwrap_or_else(|_| "127.0.0.1:7373".to_string())
        .parse()
        .unwrap();
    tracing::info!("Shirei listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

// git helpers used by discover_project (best-effort)
fn canonicalize_lossy(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

async fn git_remote(repo: &str) -> Option<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("config")
        .arg("--get")
        .arg("remote.origin.url")
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

async fn default_branch(repo: &str) -> Option<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("symbolic-ref")
        .arg("refs/remotes/origin/HEAD")
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    s.split('/').next_back().map(|x| x.to_string())
}
