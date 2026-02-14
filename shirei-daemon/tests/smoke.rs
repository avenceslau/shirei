use std::process::Stdio;
use std::time::Duration;
use std::{net::TcpListener, path::Path};
use tempfile::TempDir;

#[tokio::test]
async fn daemon_starts_and_creates_job_with_mock_opencode() -> anyhow::Result<()> {
    let td = TempDir::new()?;
    init_git_repo(td.path())?;
    let state_dir = td.path().join("state");
    tokio::fs::create_dir_all(&state_dir).await?;

    // mock opencode binary
    let bin_dir = td.path().join("bin");
    tokio::fs::create_dir_all(&bin_dir).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        use tokio::io::AsyncWriteExt;

        let mock = bin_dir.join("opencode");
        let mut f = tokio::fs::File::create(&mock).await?;
        f.write_all(
            br#"#!/usr/bin/env bash
echo "hello from mock opencode"
exit 0
"#,
        )
        .await?;
        let mut perms = tokio::fs::metadata(&mock).await?.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&mock, perms).await?;
    }

    tokio::fs::write(
        state_dir.join("config.json"),
        r#"{
  "commands": {
    "opencode": ["opencode"]
  }
}"#,
    )
    .await?;

    // start daemon
    let bind = format!("127.0.0.1:{}", free_port()?);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_shirei-daemon"))
        .env("SHIREI_STATE_DIR", state_dir.to_string_lossy().to_string())
        .env("SHIREI_BIND", &bind)
        .env("SHIREI_GITHUB_TOKEN", "dummy") // not used in this smoke test
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.to_string_lossy(),
                std::env::var("PATH").unwrap()
            ),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let token = read_token(&state_dir).await?;

    let client = reqwest::Client::new();
    let base = format!("http://{bind}");

    wait_health(&client, &base).await?;
    let v: serde_json::Value = client
        .get(format!("{}/api/v0.1/health", base))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(v["status"].as_str().unwrap(), "ok");

    // discover project (use temp dir path, may not be a git repo; discover uses git subprocess best-effort)
    let proj: serde_json::Value = client
        .post(format!("{}/api/v0.1/projects/discover", base))
        .header("Authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"root_path": td.path().to_string_lossy().to_string()}))
        .send()
        .await?
        .json()
        .await?;

    let project_id = proj["project"]["project_id"].as_str().unwrap().to_string();

    // spawn job
    let job: serde_json::Value = client
        .post(format!("{}/api/v0.1/projects/{}/jobs", base, project_id))
        .header("Authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"name":"t1","prompt":"hi"}))
        .send()
        .await?
        .json()
        .await?;

    let job_id = job["job"]["job_id"].as_str().unwrap().to_string();

    let attach: serde_json::Value = client
        .post(format!("{}/api/v0.1/jobs/{}/attach", base, job_id))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(attach["attach"]["job_id"].as_str(), Some(job_id.as_str()));
    assert_eq!(attach["attach"]["cmd"].as_str(), Some("opencode"));

    tokio::time::sleep(Duration::from_millis(1200)).await;

    // logs
    let logs: serde_json::Value = client
        .get(format!(
            "{}/api/v0.1/jobs/{}/logs?tail_lines=200",
            base, job_id
        ))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await?
        .json()
        .await?;

    let lines = logs["lines"].as_array().unwrap();
    assert!(lines.iter().any(|l| l
        .as_str()
        .unwrap_or("")
        .contains("hello from mock opencode")));

    let _ = child.kill().await;
    Ok(())
}

#[tokio::test]
async fn runner_restarts_with_continue_when_process_exits_early() -> anyhow::Result<()> {
    let td = TempDir::new()?;
    init_git_repo(td.path())?;
    let state_dir = td.path().join("state");
    tokio::fs::create_dir_all(&state_dir).await?;

    let bin_dir = td.path().join("bin");
    tokio::fs::create_dir_all(&bin_dir).await?;
    write_flaky_opencode(&bin_dir, td.path()).await?;

    let bind = format!("127.0.0.1:{}", free_port()?);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_shirei-daemon"))
        .env("SHIREI_STATE_DIR", state_dir.to_string_lossy().to_string())
        .env("SHIREI_BIND", &bind)
        .env("SHIREI_OPENCODE_CMD", "opencode")
        .env("SHIREI_GITHUB_TOKEN", "dummy")
        .env("SHIREI_CONTINUE_RETRIES", "3")
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin_dir.to_string_lossy(),
                std::env::var("PATH").unwrap()
            ),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let token = read_token(&state_dir).await?;
    let client = reqwest::Client::new();
    let base = format!("http://{bind}");
    wait_health(&client, &base).await?;

    let proj: serde_json::Value = client
        .post(format!("{}/api/v0.1/projects/discover", base))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"root_path": td.path().to_string_lossy().to_string()}))
        .send()
        .await?
        .json()
        .await?;
    let project_id = proj["project"]["project_id"].as_str().unwrap().to_string();

    let job: serde_json::Value = client
        .post(format!("{}/api/v0.1/projects/{}/jobs", base, project_id))
        .header("Authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({"name":"retry-test","prompt":"initial prompt"}))
        .send()
        .await?
        .json()
        .await?;
    let job_id = job["job"]["job_id"].as_str().unwrap().to_string();

    for _ in 0..40 {
        let jobs: serde_json::Value = client
            .get(format!("{}/api/v0.1/projects/{}/jobs", base, project_id))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await?
            .json()
            .await?;
        let status = jobs["jobs"]
            .as_array()
            .and_then(|arr| {
                arr.iter()
                    .find(|j| j["job_id"].as_str() == Some(job_id.as_str()))
            })
            .and_then(|j| j["status"].as_str())
            .unwrap_or("");
        if status == "AWAITING_REVIEW" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let logs: serde_json::Value = client
        .get(format!(
            "{}/api/v0.1/jobs/{}/logs?tail_lines=200",
            base, job_id
        ))
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await?
        .json()
        .await?;
    let lines = logs["lines"].as_array().unwrap();
    assert!(lines
        .iter()
        .any(|l| l.as_str().unwrap_or("").contains("first attempt failed")));
    assert!(lines.iter().any(|l| l
        .as_str()
        .unwrap_or("")
        .contains("continue attempt succeeded")));

    let _ = child.kill().await;
    Ok(())
}

fn free_port() -> anyhow::Result<u16> {
    let l = TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

async fn read_token(state_dir: &Path) -> anyhow::Result<String> {
    let token_path = state_dir.join("token");
    for _ in 0..40 {
        if let Ok(v) = tokio::fs::read_to_string(&token_path).await {
            return Ok(v.trim().to_string());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    anyhow::bail!("daemon did not create token file")
}

async fn wait_health(client: &reqwest::Client, base: &str) -> anyhow::Result<()> {
    for _ in 0..40 {
        if let Ok(resp) = client.get(format!("{base}/api/v0.1/health")).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    anyhow::bail!("daemon did not become healthy")
}

async fn write_flaky_opencode(bin_dir: &Path, root: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        use tokio::io::AsyncWriteExt;

        let state_file = root.join("opencode_once");
        let mock = bin_dir.join("opencode");
        let mut f = tokio::fs::File::create(&mock).await?;
        f.write_all(
            format!(
                "#!/usr/bin/env bash\nSTATE=\"{}\"\nif [ \"$1\" = \"run\" ] && [ \"$2\" = \"continue\" ]; then\n  echo \"continue attempt succeeded\"\n  exit 0\nfi\nif [ ! -f \"$STATE\" ]; then\n  touch \"$STATE\"\n  echo \"first attempt failed\"\n  exit 1\nfi\necho \"unexpected args: $*\"\nexit 1\n",
                state_file.display()
            )
            .as_bytes(),
        )
        .await?;
        let mut perms = tokio::fs::metadata(&mock).await?.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&mock, perms).await?;
    }
    Ok(())
}

fn init_git_repo(path: &Path) -> anyhow::Result<()> {
    let ok = |status: std::process::ExitStatus, step: &str| -> anyhow::Result<()> {
        if status.success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("git {} failed", step))
        }
    };
    ok(
        std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(path)
            .status()?,
        "init",
    )?;
    ok(
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .status()?,
        "config user.email",
    )?;
    ok(
        std::process::Command::new("git")
            .args(["config", "user.name", "Shirei Test"])
            .current_dir(path)
            .status()?,
        "config user.name",
    )?;
    std::fs::write(path.join("README.md"), "seed\n")?;
    ok(
        std::process::Command::new("git")
            .args(["add", "README.md"])
            .current_dir(path)
            .status()?,
        "add",
    )?;
    ok(
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(path)
            .status()?,
        "commit",
    )?;
    Ok(())
}
