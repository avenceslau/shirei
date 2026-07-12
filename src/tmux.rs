use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct TmuxSession {
    pub name: String,
    pub path: String,
    pub activity: i64,
    pub windows: u32,
    pub attached: bool,
}

pub fn list_sessions() -> Result<Vec<TmuxSession>> {
    let output = Command::new("tmux")
        .args([
            "list-sessions",
            "-F",
            "#{session_name}\t#{session_path}\t#{session_activity}\t#{session_windows}\t#{session_attached}",
        ])
        .output()
        .context("failed to run `tmux list-sessions`; is tmux installed?")?;

    if !output.status.success() {
        // tmux exits non-zero if no server is running. Treat as empty.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("no server running") || stderr.contains("error connecting") {
            return Ok(Vec::new());
        }
        return Err(anyhow!("tmux list-sessions failed: {}", stderr.trim()));
    }

    let mut sessions = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.split('\t');
        let name = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();
        let activity = parts.next().unwrap_or("0").parse().unwrap_or(0);
        let windows = parts.next().unwrap_or("0").parse().unwrap_or(0);
        let attached = parts.next().unwrap_or("0") != "0";
        if name.is_empty() {
            continue;
        }
        sessions.push(TmuxSession {
            name,
            path,
            activity,
            windows,
            attached,
        });
    }
    sessions.sort_by(|a, b| b.activity.cmp(&a.activity));
    Ok(sessions)
}

pub fn current_session_name() -> Result<Option<String>> {
    if std::env::var_os("TMUX").is_none() {
        return Ok(None);
    }

    let output = Command::new("tmux")
        .args(["display-message", "-p", "#{session_name}"])
        .output()
        .context("failed to run `tmux display-message`")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("tmux display-message failed: {}", stderr.trim()));
    }
    let name = String::from_utf8(output.stdout)
        .context("tmux display-message returned invalid utf-8")?
        .trim()
        .to_string();
    Ok((!name.is_empty()).then_some(name))
}

pub fn kill_session(name: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["kill-session", "-t", name])
        .output()
        .with_context(|| format!("failed to run tmux kill-session for {name}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "tmux kill-session failed for {name}: {}",
            stderr.trim()
        ));
    }
    Ok(())
}

pub fn new_detached(name: &str, cwd: &Path) -> Result<()> {
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", name, "-c"])
        .arg(cwd)
        .status()
        .context("failed to spawn tmux new-session")?;
    if !status.success() {
        return Err(anyhow!(
            "tmux new-session exited with status {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Run a command in a new detached window of an existing session.
///
/// The window stays open after the command exits (via `exec $SHELL`) so its
/// output and exit status remain inspectable.
pub fn run_in_new_window(session: &str, window_name: &str, cwd: &Path, cmd: &str) -> Result<()> {
    let wrapped =
        format!("{cmd}; printf '\\n[shirei] install exited %s\\n' \"$?\"; exec \"$SHELL\"");
    let status = Command::new("tmux")
        .args(["new-window", "-d", "-t", session, "-n", window_name, "-c"])
        .arg(cwd)
        .args(["sh", "-lc", &wrapped])
        .status()
        .context("failed to spawn tmux new-window")?;
    if !status.success() {
        return Err(anyhow!(
            "tmux new-window exited with status {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

pub fn switch_or_attach(name: &str) -> Result<()> {
    let action = if std::env::var_os("TMUX").is_some() {
        "switch-client"
    } else {
        "attach-session"
    };
    let status = Command::new("tmux")
        .args([action, "-t", name])
        .status()
        .with_context(|| format!("failed to run tmux {action}"))?;
    if !status.success() {
        return Err(anyhow!(
            "tmux {} exited with status {}",
            action,
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Sanitize a name hint into a tmux-safe session name. tmux disallows `.` and `:`.
pub fn sanitize_name(hint: &str) -> String {
    let cleaned: String = hint
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "shirei".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_path_separators() {
        assert_eq!(sanitize_name("foo/bar"), "foo-bar");
    }

    #[test]
    fn sanitize_strips_dots_and_colons() {
        assert_eq!(sanitize_name("my.proj:1"), "my-proj-1");
    }

    #[test]
    fn sanitize_empty_falls_back() {
        assert_eq!(sanitize_name("///"), "shirei");
    }
}
