use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use tempfile::TempDir;

struct ChildGuard {
    child: tokio::process::Child,
}

impl ChildGuard {
    async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[tokio::test]
async fn full_cli_api_flow_with_mock_opencode() -> Result<()> {
    let td = TempDir::new()?;
    init_git_repo(td.path())?;
    let state_dir = td.path().join("state");
    tokio::fs::create_dir_all(&state_dir).await?;

    let bin_dir = td.path().join("bin");
    tokio::fs::create_dir_all(&bin_dir).await?;
    write_mock_opencode(&bin_dir).await?;
    write_mock_editor(&bin_dir).await?;

    let bind = format!("127.0.0.1:{}", free_tcp_port()?);
    let mut daemon = ChildGuard {
        child: tokio::process::Command::new(env!("CARGO_BIN_EXE_shirei-daemon"))
            .env("SHIREI_STATE_DIR", state_dir.to_string_lossy().to_string())
            .env("SHIREI_BIND", &bind)
            .env("SHIREI_OPENCODE_CMD", "opencode")
            .env("SHIREI_GITHUB_TOKEN", "dummy")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.to_string_lossy(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    };

    let token = wait_for_token(&state_dir.join("token")).await?;
    let base = format!("http://{bind}");
    wait_for_health(&base).await?;

    let client = reqwest::Client::new();
    let info_status = client
        .get(format!("{}/api/v0.1/info", base))
        .send()
        .await?
        .status();
    assert_eq!(info_status, reqwest::StatusCode::UNAUTHORIZED);

    let cli = workspace_bin("shirei")?;

    let discovered = cli_json(
        &cli,
        &base,
        &token,
        &["project-discover", td.path().to_string_lossy().as_ref()],
    )?;
    let project_id = discovered["project"]["project_id"]
        .as_str()
        .ok_or_else(|| anyhow!("missing project_id"))?
        .to_string();

    let projects = cli_json(&cli, &base, &token, &["projects"])?;
    assert!(projects["projects"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .any(|p| p["project_id"].as_str() == Some(project_id.as_str()))
        })
        .unwrap_or(false));

    let job = cli_json(
        &cli,
        &base,
        &token,
        &["spawn", &project_id, "e2e", "mock prompt"],
    )?;
    let job_id = job["job"]["job_id"]
        .as_str()
        .ok_or_else(|| anyhow!("missing job_id"))?
        .to_string();
    let expected_worktree = job["job"]["worktree_path"]
        .as_str()
        .ok_or_else(|| anyhow!("missing worktree_path"))?
        .to_string();

    let editor_log = td.path().join("editor.log");
    let config_path = td.path().join("shirei-config.toml");
    tokio::fs::write(
        &config_path,
        "[editor]\ncommand = \"mock-editor --target {path}\"\n",
    )
    .await?;

    let edit_out = std::process::Command::new(&cli)
        .arg("--base-url")
        .arg(&base)
        .arg("--token")
        .arg(&token)
        .args(["jobs", &job_id, "edit"])
        .env("SHIREI_CONFIG", config_path.to_string_lossy().to_string())
        .env(
            "SHIREI_EDITOR_LOG",
            editor_log.to_string_lossy().to_string(),
        )
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.to_string_lossy(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .output()
        .with_context(|| format!("failed running {}", cli.display()))?;
    if !edit_out.status.success() {
        return Err(anyhow!(
            "cli edit failed (status={}): {}",
            edit_out.status,
            String::from_utf8_lossy(&edit_out.stderr)
        ));
    }
    let edited_path = tokio::fs::read_to_string(&editor_log).await?;
    let expected = normalize_for_compare(std::fs::canonicalize(expected_worktree)?);
    let actual = normalize_for_compare(std::fs::canonicalize(edited_path.trim())?);
    assert_eq!(actual, expected);

    let logs = wait_for_logs(&cli, &base, &token, &job_id).await?;
    assert!(logs["lines"]
        .as_array()
        .map(|arr| {
            arr.iter().any(|l| {
                l.as_str()
                    .unwrap_or("")
                    .contains("mock opencode args: run mock prompt --agent shirei")
            })
        })
        .unwrap_or(false));

    let attach = std::process::Command::new(&cli)
        .arg("--base-url")
        .arg(&base)
        .arg("--token")
        .arg(&token)
        .args(["jobs", &job_id, "attach"])
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.to_string_lossy(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .output()
        .with_context(|| format!("failed running {}", cli.display()))?;
    assert!(attach.status.success());
    let attach_stdout = String::from_utf8(attach.stdout)?;
    assert!(attach_stdout.contains("mock opencode args:"));
    assert!(!attach_stdout.contains("run mock prompt"));

    client
        .post(format!("{}/api/v0.1/jobs/{}/ack", base, job_id))
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()?;

    let jobs_after_ack = cli_json(&cli, &base, &token, &["jobs", &project_id])?;
    assert!(jobs_after_ack["jobs"]
        .as_array()
        .map(|arr| {
            arr.iter().any(|j| {
                j["job_id"].as_str() == Some(job_id.as_str())
                    && j["status"].as_str() == Some("DONE_APPROVED")
            })
        })
        .unwrap_or(false));

    client
        .post(format!("{}/api/v0.1/jobs/{}/unack", base, job_id))
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()?;

    let jobs_after_unack = cli_json(&cli, &base, &token, &["jobs", &project_id])?;
    assert!(jobs_after_unack["jobs"]
        .as_array()
        .map(|arr| {
            arr.iter().any(|j| {
                j["job_id"].as_str() == Some(job_id.as_str())
                    && j["status"].as_str() == Some("AWAITING_REVIEW")
            })
        })
        .unwrap_or(false));

    daemon.kill().await;
    Ok(())
}

#[tokio::test]
async fn cli_lifecycle_and_tui_commands_work() -> Result<()> {
    let td = TempDir::new()?;
    let state_dir = td.path().join("state");
    tokio::fs::create_dir_all(&state_dir).await?;

    let bin_dir = td.path().join("bin");
    tokio::fs::create_dir_all(&bin_dir).await?;
    write_mock_tui(&bin_dir).await?;

    let bind = format!("127.0.0.1:{}", free_tcp_port()?);
    let base = format!("http://{bind}");

    let cli = workspace_bin("shirei")?;
    let daemon = workspace_bin("shirei-daemon")?;
    let tui_log = td.path().join("tui.log");

    let start = std::process::Command::new(&cli)
        .arg("--base-url")
        .arg(&base)
        .arg("--state-dir")
        .arg(state_dir.to_string_lossy().to_string())
        .arg("start")
        .env("SHIREI_DAEMON_BIN", daemon.to_string_lossy().to_string())
        .output()
        .with_context(|| format!("failed running {}", cli.display()))?;
    if !start.status.success() {
        return Err(anyhow!(
            "cli start failed (status={}): {}",
            start.status,
            String::from_utf8_lossy(&start.stderr)
        ));
    }

    let token = wait_for_token(&state_dir.join("token")).await?;
    wait_for_health(&base).await?;

    let start_again = std::process::Command::new(&cli)
        .arg("--base-url")
        .arg(&base)
        .arg("--state-dir")
        .arg(state_dir.to_string_lossy().to_string())
        .arg("start")
        .env("SHIREI_DAEMON_BIN", daemon.to_string_lossy().to_string())
        .output()
        .with_context(|| format!("failed running {}", cli.display()))?;
    assert!(start_again.status.success());

    let restart = std::process::Command::new(&cli)
        .arg("--base-url")
        .arg(&base)
        .arg("--state-dir")
        .arg(state_dir.to_string_lossy().to_string())
        .arg("restart")
        .env("SHIREI_DAEMON_BIN", daemon.to_string_lossy().to_string())
        .output()
        .with_context(|| format!("failed running {}", cli.display()))?;
    if !restart.status.success() {
        return Err(anyhow!(
            "cli restart failed (status={}): {}",
            restart.status,
            String::from_utf8_lossy(&restart.stderr)
        ));
    }
    wait_for_health(&base).await?;

    let tui = std::process::Command::new(&cli)
        .arg("--base-url")
        .arg(&base)
        .arg("--state-dir")
        .arg(state_dir.to_string_lossy().to_string())
        .args(["tui", "--", "--discover-path", "."])
        .env(
            "SHIREI_TUI_BIN",
            bin_dir.join("mock-tui").to_string_lossy().to_string(),
        )
        .env("SHIREI_TUI_LOG", tui_log.to_string_lossy().to_string())
        .output()
        .with_context(|| format!("failed running {}", cli.display()))?;
    if !tui.status.success() {
        return Err(anyhow!(
            "cli tui failed (status={}): {}",
            tui.status,
            String::from_utf8_lossy(&tui.stderr)
        ));
    }
    let tui_args = tokio::fs::read_to_string(&tui_log).await?;
    assert!(tui_args.contains(&format!("--base-url {base}")));
    assert!(tui_args.contains(&format!("--token {token}")));
    assert!(tui_args.contains("--discover-path ."));

    let stop = std::process::Command::new(&cli)
        .arg("--base-url")
        .arg(&base)
        .arg("--state-dir")
        .arg(state_dir.to_string_lossy().to_string())
        .arg("stop")
        .output()
        .with_context(|| format!("failed running {}", cli.display()))?;
    if !stop.status.success() {
        return Err(anyhow!(
            "cli stop failed (status={}): {}",
            stop.status,
            String::from_utf8_lossy(&stop.stderr)
        ));
    }

    let stop_again = std::process::Command::new(&cli)
        .arg("--base-url")
        .arg(&base)
        .arg("--state-dir")
        .arg(state_dir.to_string_lossy().to_string())
        .arg("stop")
        .output()
        .with_context(|| format!("failed running {}", cli.display()))?;
    assert!(stop_again.status.success());

    Ok(())
}

fn workspace_bin(name: &str) -> Result<PathBuf> {
    let package = match name {
        "shirei" => "shirei",
        "shirei-daemon" => "shirei-daemon",
        other => other,
    };
    let status = std::process::Command::new("cargo")
        .args(["build", "-p", package])
        .status()
        .with_context(|| format!("failed to build package {package}"))?;
    if !status.success() {
        return Err(anyhow!("cargo build -p {package} failed"));
    }

    let daemon_path = PathBuf::from(env!("CARGO_BIN_EXE_shirei-daemon"));
    let deps_dir = daemon_path
        .parent()
        .ok_or_else(|| anyhow!("missing daemon binary parent"))?;
    let debug_dir = if deps_dir.file_name().and_then(|s| s.to_str()) == Some("deps") {
        deps_dir
            .parent()
            .ok_or_else(|| anyhow!("missing debug dir"))?
    } else {
        deps_dir
    };

    let exe_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = debug_dir.join(exe_name);
    if !path.exists() {
        return Err(anyhow!("binary not found: {}", path.display()));
    }
    Ok(path)
}

fn init_git_repo(path: &Path) -> Result<()> {
    let run = |args: &[&str], step: &str| -> Result<()> {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .with_context(|| format!("git {step} failed to start"))?;
        if status.success() {
            Ok(())
        } else {
            Err(anyhow!("git {step} failed"))
        }
    };
    run(&["init", "-b", "main"], "init")?;
    run(
        &["config", "user.email", "test@example.com"],
        "config email",
    )?;
    run(&["config", "user.name", "Shirei Test"], "config name")?;
    std::fs::write(path.join("README.md"), "seed\n")?;
    run(&["add", "README.md"], "add")?;
    run(&["commit", "-m", "init"], "commit")?;
    Ok(())
}

async fn write_mock_opencode(bin_dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        use tokio::io::AsyncWriteExt;

        let mock = bin_dir.join("opencode");
        let mut f = tokio::fs::File::create(&mock).await?;
        f.write_all(
            br#"#!/usr/bin/env bash
echo "mock opencode args: $*"
exit 0
"#,
        )
        .await?;
        let mut perms = tokio::fs::metadata(&mock).await?.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&mock, perms).await?;
    }
    Ok(())
}

async fn write_mock_editor(bin_dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        use tokio::io::AsyncWriteExt;

        let mock = bin_dir.join("mock-editor");
        let mut f = tokio::fs::File::create(&mock).await?;
        f.write_all(
            br#"#!/usr/bin/env bash
target=""
while [[ $# -gt 0 ]]; do
  if [[ "$1" == "--target" ]]; then
    target="$2"
    shift 2
  else
    target="$1"
    shift
  fi
done
if [[ -n "${SHIREI_EDITOR_LOG:-}" ]]; then
  echo "$target" > "$SHIREI_EDITOR_LOG"
fi
exit 0
"#,
        )
        .await?;
        let mut perms = tokio::fs::metadata(&mock).await?.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&mock, perms).await?;
    }
    Ok(())
}

async fn write_mock_tui(bin_dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        use tokio::io::AsyncWriteExt;

        let mock = bin_dir.join("mock-tui");
        let mut f = tokio::fs::File::create(&mock).await?;
        f.write_all(
            br#"#!/usr/bin/env bash
if [[ -n "${SHIREI_TUI_LOG:-}" ]]; then
  echo "$*" > "$SHIREI_TUI_LOG"
fi
exit 0
"#,
        )
        .await?;
        let mut perms = tokio::fs::metadata(&mock).await?.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&mock, perms).await?;
    }
    Ok(())
}

fn free_tcp_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

async fn wait_for_token(path: &Path) -> Result<String> {
    for _ in 0..40 {
        if let Ok(v) = tokio::fs::read_to_string(path).await {
            let token = v.trim().to_string();
            if !token.is_empty() {
                return Ok(token);
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(anyhow!("daemon did not write token"))
}

async fn wait_for_health(base: &str) -> Result<()> {
    let client = reqwest::Client::new();
    for _ in 0..60 {
        let res = client.get(format!("{}/api/v0.1/health", base)).send().await;
        if let Ok(resp) = res {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    Err(anyhow!("daemon did not become healthy"))
}

async fn wait_for_logs(cli: &Path, base: &str, token: &str, job_id: &str) -> Result<Value> {
    for _ in 0..50 {
        let logs = cli_json(cli, base, token, &["logs", job_id, "--tail-lines", "200"])?;
        if logs["lines"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .any(|l| l.as_str().unwrap_or("").contains("mock opencode args:"))
            })
            .unwrap_or(false)
        {
            return Ok(logs);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(anyhow!("job logs did not contain mock output"))
}

fn cli_json(cli_bin: &Path, base: &str, token: &str, args: &[&str]) -> Result<Value> {
    let out = std::process::Command::new(cli_bin)
        .arg("--base-url")
        .arg(base)
        .arg("--token")
        .arg(token)
        .args(args)
        .output()
        .with_context(|| format!("failed running {}", cli_bin.display()))?;

    if !out.status.success() {
        return Err(anyhow!(
            "cli failed (status={}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    let stdout = String::from_utf8(out.stdout)?;
    let v = serde_json::from_str::<Value>(&stdout)
        .with_context(|| format!("invalid json from cli output: {stdout}"))?;
    Ok(v)
}

fn normalize_for_compare(path: PathBuf) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let path_str = path.to_string_lossy();
        if let Some(stripped) = path_str.strip_prefix("/private") {
            return PathBuf::from(stripped);
        }
    }
    path
}
