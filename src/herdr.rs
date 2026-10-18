use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::session::Session;

#[derive(Deserialize)]
struct SnapshotResponse {
    result: SnapshotResult,
}

#[derive(Deserialize)]
struct SnapshotResult {
    snapshot: Snapshot,
}

#[derive(Deserialize)]
struct Snapshot {
    workspaces: Vec<Workspace>,
    panes: Vec<Pane>,
}

#[derive(Deserialize)]
struct Workspace {
    workspace_id: String,
    label: String,
    tab_count: u32,
    focused: bool,
}

#[derive(Deserialize)]
struct Pane {
    workspace_id: String,
    cwd: String,
}

#[derive(Deserialize)]
struct WorkspaceResponse {
    result: WorkspaceResult,
}

#[derive(Deserialize)]
struct WorkspaceResult {
    workspace: OpenedWorkspace,
}

#[derive(Deserialize)]
struct OpenedWorkspace {
    workspace_id: String,
}

pub fn list_workspaces() -> Result<Vec<Session>> {
    let Some(response) = snapshot()? else {
        return Ok(Vec::new());
    };

    Ok(response
        .result
        .snapshot
        .workspaces
        .into_iter()
        .map(|workspace| {
            let path = response
                .result
                .snapshot
                .panes
                .iter()
                .find(|pane| pane.workspace_id == workspace.workspace_id)
                .map(|pane| pane.cwd.clone())
                .unwrap_or_default();
            Session {
                id: workspace.workspace_id,
                name: workspace.label,
                path,
                units: workspace.tab_count,
                attached: workspace.focused,
            }
        })
        .collect())
}

pub fn create_workspace(name: &str, path: &Path) -> Result<String> {
    ensure_server()?;

    let output = Command::new("herdr")
        .args(["workspace", "create", "--cwd"])
        .arg(path)
        .args(["--label", name, "--no-focus"])
        .output()
        .context("failed to run `herdr workspace create`")?;
    let response: WorkspaceResponse = parse_output(output, "herdr workspace create")?;
    Ok(response.result.workspace.workspace_id)
}

pub fn open_worktree(name: &str, repo: &Path, path: &Path) -> Result<String> {
    ensure_server()?;

    let output = Command::new("herdr")
        .args(["worktree", "open", "--cwd"])
        .arg(repo)
        .arg("--path")
        .arg(path)
        .args(["--label", name, "--no-focus"])
        .output()
        .context("failed to run `herdr worktree open`")?;
    let response: WorkspaceResponse = parse_output(output, "herdr worktree open")?;
    Ok(response.result.workspace.workspace_id)
}

pub fn focus_or_attach(id: &str) -> Result<()> {
    run_status(
        Command::new("herdr")
            .args(["workspace", "focus", id])
            .stdout(Stdio::null()),
        "herdr workspace focus",
    )?;
    if std::env::var_os("HERDR_ENV").is_none() {
        run_status(&mut Command::new("herdr"), "herdr attach")?;
    }
    Ok(())
}

pub fn close_workspace(id: &str) -> Result<()> {
    run_status(
        Command::new("herdr")
            .args(["workspace", "close", id])
            .stdout(Stdio::null()),
        "herdr workspace close",
    )
}

pub fn run_in_new_tab(workspace: &str, path: &Path, cmd: &str) -> Result<()> {
    let output = Command::new("herdr")
        .args(["tab", "create", "--workspace", workspace, "--cwd"])
        .arg(path)
        .args(["--label", "install", "--no-focus"])
        .output()
        .context("failed to run `herdr tab create`")?;
    let value: serde_json::Value = parse_output(output, "herdr tab create")?;
    let pane = value["result"]["root_pane"]["pane_id"]
        .as_str()
        .ok_or_else(|| anyhow!("herdr tab create returned no pane id"))?;
    run_status(
        Command::new("herdr")
            .args(["pane", "run", pane, cmd])
            .stdout(Stdio::null()),
        "herdr pane run",
    )
}

fn parse_output<T: serde::de::DeserializeOwned>(output: Output, command: &str) -> Result<T> {
    if !output.status.success() {
        return Err(anyhow!(
            "{command} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout).with_context(|| format!("invalid JSON from {command}"))
}

fn snapshot() -> Result<Option<SnapshotResponse>> {
    let output = Command::new("herdr")
        .args(["api", "snapshot"])
        .output()
        .context("failed to run `herdr api snapshot`; is herdr installed?")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("No such file or directory") || stderr.contains("not running") {
            return Ok(None);
        }
    }
    parse_output(output, "herdr api snapshot").map(Some)
}

fn ensure_server() -> Result<()> {
    if snapshot()?.is_some() {
        return Ok(());
    }

    #[cfg(unix)]
    let mut server = {
        let mut command = Command::new("nohup");
        command.args(["herdr", "server"]).process_group(0);
        command
    };
    #[cfg(not(unix))]
    let mut server = {
        let mut command = Command::new("herdr");
        command.arg("server");
        command
    };
    server
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start herdr server")?;

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if snapshot()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "herdr server did not become ready within 15 seconds; check ~/.config/herdr/herdr-server.log"
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn run_status(command: &mut Command, label: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to run {label}"))?;
    if !status.success() {
        return Err(anyhow!(
            "{label} exited with status {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_contract_deserializes() {
        let response: SnapshotResponse = serde_json::from_str(
            r#"{
                "result": {
                    "snapshot": {
                        "workspaces": [{
                            "workspace_id": "w1",
                            "label": "shirei",
                            "tab_count": 2,
                            "focused": true
                        }],
                        "panes": [{
                            "workspace_id": "w1",
                            "cwd": "/tmp/shirei"
                        }]
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(response.result.snapshot.workspaces[0].workspace_id, "w1");
        assert_eq!(response.result.snapshot.panes[0].cwd, "/tmp/shirei");
    }
}
