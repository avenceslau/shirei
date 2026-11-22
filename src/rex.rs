use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::{Command, Output};

use crate::session::Session;

const TERMINAL_CREATOR: &str = "com.superlogical.terminal";

#[derive(Deserialize)]
struct ListResponse {
    sessions: Vec<SessionSummary>,
}

#[derive(Deserialize)]
struct SessionSummary {
    session_id: String,
    label: String,
}

#[derive(Deserialize)]
struct SessionView {
    windows: Vec<Window>,
}

#[derive(Deserialize)]
struct Window {
    layers: Vec<Layer>,
}

#[derive(Deserialize)]
struct Layer {
    blocks: Vec<Block>,
}

#[derive(Deserialize)]
struct Block {
    block_id: String,
    creator_name: String,
}

#[derive(Deserialize)]
struct ProcessResponse {
    foreground: Option<Process>,
    child: Option<Process>,
}

#[derive(Deserialize)]
struct Process {
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct CreateResponse {
    session_id: String,
}

#[derive(Deserialize)]
struct BlockInspectResponse {
    block: InspectedBlock,
}

#[derive(Deserialize)]
struct InspectedBlock {
    owner: Option<String>,
}

pub fn list_sessions() -> Result<Vec<Session>> {
    let output = Command::new("rex")
        .args(["ls", "--json"])
        .output()
        .context("failed to run `rex ls`; is rex installed?")?;
    let response: ListResponse = parse_output(output, "rex ls")?;
    let current = std::env::var("REX_SESSION").ok();

    response
        .sessions
        .into_iter()
        .map(|summary| {
            let view = inspect_session(&summary.session_id)?;
            let path = session_path(&summary.session_id, &view);
            Ok(Session {
                attached: current.as_deref() == Some(summary.session_id.as_str()),
                id: summary.session_id,
                name: summary.label,
                path,
                units: view.windows.len().try_into().unwrap_or(u32::MAX),
            })
        })
        .collect()
}

pub fn current_session_id() -> Result<Option<String>> {
    Ok(std::env::var("REX_SESSION").ok())
}

pub fn create_session(name: &str, path: &Path) -> Result<String> {
    let output = Command::new("rex")
        .args(["new", "--json", "--window", "Terminal", "--cwd"])
        .arg(path)
        .arg(name)
        .output()
        .context("failed to run `rex new`; is rex installed?")?;
    let response: CreateResponse = parse_output(output, "rex new")?;
    Ok(response.session_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Right,
    Below,
}

impl SplitDirection {
    fn as_arg(self) -> &'static str {
        match self {
            SplitDirection::Right => "right",
            SplitDirection::Below => "below",
        }
    }
}

/// Open a shell beside `anchor_block`, or beside the picker's own block when it
/// is `None`; no Rex.app action or Remote Control is needed.
pub fn open_directory_split(
    path: &Path,
    anchor_block: Option<&str>,
    direction: SplitDirection,
) -> Result<()> {
    let session = std::env::var("REX_SESSION")
        .ok()
        .filter(|id| !id.is_empty())
        .context("rex_picker_split requires running Shirei inside a Rex terminal")?;
    let anchor_block = anchor_block
        .filter(|block| !block.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("REX_BLOCK")
                .ok()
                .filter(|block| !block.is_empty())
        });

    let mut command = Command::new("rex");
    command.args(["split", "--session", &session]);
    if let Some(block) = &anchor_block {
        command.args(["--block", block]);
    }
    let output = command
        .args(["--split", direction.as_arg()])
        .arg("--cwd")
        .arg(path)
        .arg("--json")
        .output()
        .context("failed to run `rex split`; is rex installed?")?;
    ensure_success(output, "rex split")
}

pub fn select_session(id: &str) -> Result<()> {
    let mut command = Command::new("rex");
    if let Some(client) = current_client() {
        command.args(["--client", &client]);
    }
    let output = command
        .args(["do", "session.select"])
        .arg(format!("session_id={id}"))
        .output()
        .context("failed to run `rex do session.select`; is rex installed?")?;
    ensure_success(output, "rex do session.select")
}

pub fn kill_session(id: &str) -> Result<()> {
    let output = Command::new("rex")
        .args(["kill", id])
        .output()
        .context("failed to run `rex kill`; is rex installed?")?;
    ensure_success(output, "rex kill")
}

pub fn run_in_new_window(session: &str, path: &Path, cmd: &str) -> Result<()> {
    let output = Command::new("rex")
        .args(["window", "new", "--session", session, "--cwd"])
        .arg(path)
        .args([
            "--keep-open",
            "--focus=false",
            "install",
            "--",
            "sh",
            "-lc",
            cmd,
        ])
        .output()
        .context("failed to run `rex window new`; is rex installed?")?;
    ensure_success(output, "rex window new")
}

fn inspect_session(id: &str) -> Result<SessionView> {
    let output = Command::new("rex")
        .args(["session", "inspect", id, "--json"])
        .output()
        .with_context(|| format!("failed to inspect Rex session {id}"))?;
    parse_output(output, "rex session inspect")
}

fn session_path(session: &str, view: &SessionView) -> String {
    view.windows
        .iter()
        .flat_map(|window| &window.layers)
        .flat_map(|layer| &layer.blocks)
        .filter(|block| block.creator_name == TERMINAL_CREATOR)
        .find_map(|block| block_path(session, &block.block_id))
        .unwrap_or_default()
}

fn block_path(session: &str, block: &str) -> Option<String> {
    let output = Command::new("rex")
        .args([
            "block",
            "call",
            "--session",
            session,
            "--block",
            block,
            "process",
        ])
        .output()
        .ok()?;
    let response: ProcessResponse = parse_output(output, "rex block call process").ok()?;
    response
        .foreground
        .and_then(|process| process.cwd)
        .or_else(|| response.child.and_then(|process| process.cwd))
}

fn current_client() -> Option<String> {
    let block = std::env::var("REX_BLOCK").ok()?;
    let output = Command::new("rex")
        .args(["block", "inspect", &block, "--json"])
        .output()
        .ok()?;
    let response: BlockInspectResponse = parse_output(output, "rex block inspect").ok()?;
    response.block.owner
}

fn parse_output<T: serde::de::DeserializeOwned>(output: Output, command: &str) -> Result<T> {
    if !output.status.success() {
        return Err(command_error(&output, command));
    }
    serde_json::from_slice(&output.stdout).with_context(|| format!("invalid JSON from {command}"))
}

fn ensure_success(output: Output, command: &str) -> Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error(&output, command))
    }
}

fn command_error(output: &Output, command: &str) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    if detail.is_empty() {
        anyhow!(
            "{command} exited with status {}",
            output.status.code().unwrap_or(-1)
        )
    } else {
        anyhow!("{command} failed: {detail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_contract_deserializes() {
        let response: ListResponse = serde_json::from_str(
            r#"{
                "sessions": [{
                    "session_id": "session:1",
                    "label": "shirei"
                }]
            }"#,
        )
        .unwrap();

        assert_eq!(response.sessions[0].session_id, "session:1");
        assert_eq!(response.sessions[0].label, "shirei");
    }

    #[test]
    fn inspect_contract_deserializes() {
        let view: SessionView = serde_json::from_str(
            r#"{
                "revision": 1,
                "session_id": "session:1",
                "label": "shirei",
                "windows": [{
                    "window_id": "window:1",
                    "layers": [{
                        "layer_id": "layer:1",
                        "kind": "tiled",
                        "blocks": [{
                            "block_id": "block:1",
                            "creator_name": "com.superlogical.terminal",
                            "label": "shell"
                        }]
                    }]
                }]
            }"#,
        )
        .unwrap();

        assert_eq!(view.windows.len(), 1);
        assert_eq!(view.windows[0].layers[0].blocks[0].block_id, "block:1");
    }

    #[test]
    fn process_contract_prefers_foreground_cwd() {
        let response: ProcessResponse = serde_json::from_str(
            r#"{
                "last_exit": null,
                "child": {"name": "login", "cwd": "/tmp/child"},
                "foreground": {"name": "zsh", "cwd": "/tmp/project"}
            }"#,
        )
        .unwrap();

        let path = response
            .foreground
            .and_then(|process| process.cwd)
            .or_else(|| response.child.and_then(|process| process.cwd));
        assert_eq!(path.as_deref(), Some("/tmp/project"));
    }
}
