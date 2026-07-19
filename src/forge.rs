use anyhow::{anyhow, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug)]
pub enum ReviewStatus {
    Covered,
    Stale,
    NoneFound,
    Unknown(Vec<String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ForgeKind {
    GitHub,
    GitLab,
    Unknown,
}

struct RemoteTarget {
    display: String,
    gh_repo: String,
    glab_repo: String,
    kind: ForgeKind,
}

fn remote_target(remote: &str) -> Option<RemoteTarget> {
    let without_query = remote.split(['?', '#']).next().unwrap_or(remote);
    let trimmed = without_query.trim_end_matches(".git");
    if let Some((scheme, rest)) = trimmed.split_once("://") {
        let rest = rest
            .rsplit_once('@')
            .map(|(_, value)| value)
            .unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        if host.is_empty() || path.is_empty() {
            return None;
        }
        let lowercase_host = host.to_ascii_lowercase();
        let kind = if lowercase_host.contains("github") {
            ForgeKind::GitHub
        } else if lowercase_host.contains("gitlab") {
            ForgeKind::GitLab
        } else {
            ForgeKind::Unknown
        };
        return Some(RemoteTarget {
            display: format!("{host}/{path}"),
            gh_repo: format!("{host}/{path}"),
            glab_repo: format!("{scheme}://{host}/{path}"),
            kind,
        });
    }

    let (host, path) = trimmed.split_once(':')?;
    if host.contains('/') || path.is_empty() {
        return None;
    }
    let host = host
        .rsplit_once('@')
        .map(|(_, value)| value)
        .unwrap_or(host);
    if host.is_empty() {
        return None;
    }
    let lowercase_host = host.to_ascii_lowercase();
    let kind = if lowercase_host.contains("github") {
        ForgeKind::GitHub
    } else if lowercase_host.contains("gitlab") {
        ForgeKind::GitLab
    } else {
        ForgeKind::Unknown
    };
    Some(RemoteTarget {
        display: format!("{host}/{path}"),
        gh_repo: format!("{host}/{path}"),
        glab_repo: format!("ssh://git@{host}/{path}"),
        kind,
    })
}

fn is_ancestor(worktree: &Path, ancestor: &str, descendant: &str) -> Option<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

pub fn review_status(worktree: &Path, branch: &str) -> ReviewStatus {
    let remote_output = match Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["remote", "-v"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            return ReviewStatus::Unknown(vec![format!(
                "git remote -v failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )]);
        }
        Err(error) => {
            return ReviewStatus::Unknown(vec![format!("git remote -v failed: {error}")]);
        }
    };

    let head_output = match Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "HEAD"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            return ReviewStatus::Unknown(vec![format!(
                "git rev-parse HEAD failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )]);
        }
        Err(error) => {
            return ReviewStatus::Unknown(vec![format!("git rev-parse HEAD failed: {error}")]);
        }
    };
    let head = String::from_utf8_lossy(&head_output.stdout)
        .trim()
        .to_string();

    let remotes: BTreeMap<String, RemoteTarget> = String::from_utf8_lossy(&remote_output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(remote_target)
        .map(|target| (target.display.clone(), target))
        .collect();
    if remotes.is_empty() {
        return ReviewStatus::Unknown(vec!["repository has no supported remotes".to_string()]);
    }

    let mut covered = 0usize;
    let mut errors = Vec::new();
    let mut ambiguous_review = false;
    let mut stale_review = false;
    for target in remotes.values() {
        let mut remote_covered = false;
        if !matches!(target.kind, ForgeKind::GitLab) {
            match Command::new("gh")
                .args([
                    "pr",
                    "list",
                    "--repo",
                    &target.gh_repo,
                    "--head",
                    branch,
                    "--state",
                    "all",
                    "--limit",
                    "100",
                    "--json",
                    "state,mergedAt,headRefOid",
                ])
                .current_dir(worktree)
                .env("GH_PROMPT_DISABLED", "1")
                .env("GH_HTTP_TIMEOUT", "15")
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdin(Stdio::null())
                .output()
            {
                Ok(output) if output.status.success() => {
                    match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                        Ok(value) if value.as_array().is_some() => {
                            remote_covered = true;
                            for item in value.as_array().unwrap() {
                                let open = item["state"]
                                    .as_str()
                                    .is_some_and(|state| state.eq_ignore_ascii_case("open"));
                                let merged = !item["mergedAt"].is_null();
                                if !open && !merged {
                                    continue;
                                }
                                let Some(review_head) = item["headRefOid"].as_str() else {
                                    ambiguous_review = true;
                                    errors.push(format!(
                                        "gh returned a review without headRefOid for {}",
                                        target.display
                                    ));
                                    continue;
                                };
                                let review_to_local = is_ancestor(worktree, review_head, &head);
                                let local_to_review = is_ancestor(worktree, &head, review_head);
                                if (open || merged) && local_to_review == Some(true) {
                                    return ReviewStatus::Covered;
                                }
                                if review_to_local == Some(true) {
                                    stale_review = true;
                                }
                                if review_to_local.is_none() || local_to_review.is_none() {
                                    ambiguous_review = true;
                                    errors.push(format!(
                                        "could not compare local HEAD with a gh review for {}",
                                        target.display
                                    ));
                                }
                            }
                        }
                        Ok(_) => errors
                            .push(format!("gh returned non-array JSON for {}", target.display)),
                        Err(error) => errors.push(format!(
                            "gh returned invalid JSON for {}: {error}",
                            target.display
                        )),
                    }
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    errors.push(format!(
                        "gh could not query {}: {}",
                        target.display,
                        stderr.trim()
                    ));
                }
                Err(error) => {
                    errors.push(format!("gh unavailable for {}: {error}", target.display))
                }
            }
        }

        if !matches!(target.kind, ForgeKind::GitHub) {
            match Command::new("glab")
                .args([
                    "mr",
                    "list",
                    "--repo",
                    &target.glab_repo,
                    "--source-branch",
                    branch,
                    "--all",
                    "--per-page",
                    "100",
                    "--output",
                    "json",
                ])
                .current_dir(worktree)
                .env("GLAB_PROMPT_DISABLED", "1")
                .env("GLAB_HTTP_TIMEOUT", "15")
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdin(Stdio::null())
                .output()
            {
                Ok(output) if output.status.success() => {
                    match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                        Ok(value) if value.as_array().is_some() => {
                            remote_covered = true;
                            for item in value.as_array().unwrap() {
                                let open = item["state"].as_str().is_some_and(|state| {
                                    state.eq_ignore_ascii_case("open")
                                        || state.eq_ignore_ascii_case("opened")
                                });
                                let merged = item["state"]
                                    .as_str()
                                    .is_some_and(|state| state.eq_ignore_ascii_case("merged"))
                                    || !item["merged_at"].is_null();
                                if !open && !merged {
                                    continue;
                                }
                                let Some(review_head) = item["sha"].as_str() else {
                                    ambiguous_review = true;
                                    errors.push(format!(
                                        "glab returned a review without sha for {}",
                                        target.display
                                    ));
                                    continue;
                                };
                                let review_to_local = is_ancestor(worktree, review_head, &head);
                                let local_to_review = is_ancestor(worktree, &head, review_head);
                                if (open || merged) && local_to_review == Some(true) {
                                    return ReviewStatus::Covered;
                                }
                                if review_to_local == Some(true) {
                                    stale_review = true;
                                }
                                if review_to_local.is_none() || local_to_review.is_none() {
                                    ambiguous_review = true;
                                    errors.push(format!(
                                        "could not compare local HEAD with a glab review for {}",
                                        target.display
                                    ));
                                }
                            }
                        }
                        Ok(_) => errors.push(format!(
                            "glab returned non-array JSON for {}",
                            target.display
                        )),
                        Err(error) => errors.push(format!(
                            "glab returned invalid JSON for {}: {error}",
                            target.display
                        )),
                    }
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    errors.push(format!(
                        "glab could not query {}: {}",
                        target.display,
                        stderr.trim()
                    ));
                }
                Err(error) => {
                    errors.push(format!("glab unavailable for {}: {error}", target.display))
                }
            }
        }

        if remote_covered {
            covered += 1;
        }
    }

    if ambiguous_review || covered != remotes.len() {
        ReviewStatus::Unknown(errors)
    } else if stale_review {
        ReviewStatus::Stale
    } else {
        ReviewStatus::NoneFound
    }
}

#[derive(Debug, Clone)]
pub struct ReviewRequest<'a> {
    pub worktree: &'a Path,
    pub branch: &'a str,
    pub base: &'a str,
    pub draft: bool,
}

#[derive(Debug, Clone)]
pub enum EnsureReviewOutcome {
    Created {
        url: Option<String>,
        id: Option<String>,
    },
    Updated {
        url: Option<String>,
        id: Option<String>,
    },
    AlreadyCorrect {
        url: Option<String>,
        id: Option<String>,
    },
}

impl EnsureReviewOutcome {
    pub fn id(&self) -> Option<&str> {
        match self {
            EnsureReviewOutcome::Created { id, .. }
            | EnsureReviewOutcome::Updated { id, .. }
            | EnsureReviewOutcome::AlreadyCorrect { id, .. } => id.as_deref(),
        }
    }

    pub fn url(&self) -> Option<&str> {
        match self {
            EnsureReviewOutcome::Created { url, .. }
            | EnsureReviewOutcome::Updated { url, .. }
            | EnsureReviewOutcome::AlreadyCorrect { url, .. } => url.as_deref(),
        }
    }
}

pub fn ensure_review(request: ReviewRequest<'_>) -> Result<EnsureReviewOutcome> {
    match preferred_forge(request.worktree)? {
        ForgeKind::GitLab => ensure_gitlab_review(request),
        ForgeKind::GitHub | ForgeKind::Unknown => ensure_github_review(request),
    }
}

pub fn get_review_body(worktree: &Path, review_id: &str) -> Result<String> {
    match preferred_forge(worktree)? {
        ForgeKind::GitLab => get_gitlab_review_body(worktree, review_id),
        ForgeKind::GitHub | ForgeKind::Unknown => get_github_review_body(worktree, review_id),
    }
}

pub fn update_review_body(worktree: &Path, review_id: &str, body: &str) -> Result<()> {
    match preferred_forge(worktree)? {
        ForgeKind::GitLab => update_gitlab_review_body(worktree, review_id, body),
        ForgeKind::GitHub | ForgeKind::Unknown => {
            update_github_review_body(worktree, review_id, body)
        }
    }
}

fn ensure_github_review(request: ReviewRequest<'_>) -> Result<EnsureReviewOutcome> {
    let list = Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            request.branch,
            "--state",
            "open",
            "--limit",
            "20",
            "--json",
            "number,url,baseRefName",
        ])
        .current_dir(request.worktree)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .context("failed to run gh pr list")?;
    if !list.status.success() {
        return Err(anyhow!(
            "gh pr list failed: {}",
            String::from_utf8_lossy(&list.stderr).trim()
        ));
    }
    let existing: serde_json::Value =
        serde_json::from_slice(&list.stdout).context("parse gh pr list JSON")?;
    if let Some(item) = existing.as_array().and_then(|items| items.first()) {
        let number = item["number"]
            .as_i64()
            .ok_or_else(|| anyhow!("gh pr list returned an item without number"))?
            .to_string();
        let url = item["url"].as_str().map(str::to_string);
        let current_base = item["baseRefName"].as_str().unwrap_or_default();
        if current_base != request.base {
            let edit = Command::new("gh")
                .args(["pr", "edit", &number, "--base", request.base])
                .current_dir(request.worktree)
                .env("GH_PROMPT_DISABLED", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdin(Stdio::null())
                .output()
                .context("failed to run gh pr edit")?;
            if !edit.status.success() {
                return Err(anyhow!(
                    "gh pr edit failed: {}",
                    String::from_utf8_lossy(&edit.stderr).trim()
                ));
            }
            return Ok(EnsureReviewOutcome::Updated {
                url,
                id: Some(number),
            });
        }
        return Ok(EnsureReviewOutcome::AlreadyCorrect {
            url,
            id: Some(number),
        });
    }

    let mut args = vec![
        "pr",
        "create",
        "--base",
        request.base,
        "--head",
        request.branch,
        "--fill",
    ];
    if request.draft {
        args.push("--draft");
    }
    let create = Command::new("gh")
        .args(args)
        .current_dir(request.worktree)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .context("failed to run gh pr create")?;
    if !create.status.success() {
        return Err(anyhow!(
            "gh pr create failed: {}",
            String::from_utf8_lossy(&create.stderr).trim()
        ));
    }
    let url = String::from_utf8_lossy(&create.stdout).trim().to_string();
    let viewed = view_github_review(request.worktree, request.branch)?;
    Ok(EnsureReviewOutcome::Created {
        url: (!url.is_empty()).then_some(url).or(viewed.1),
        id: viewed.0,
    })
}

fn get_github_review_body(worktree: &Path, review_id: &str) -> Result<String> {
    let output = Command::new("gh")
        .args(["pr", "view", review_id, "--json", "body"])
        .current_dir(worktree)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .context("failed to run gh pr view")?;
    if !output.status.success() {
        return Err(anyhow!(
            "gh pr view failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parse gh pr view JSON")?;
    Ok(value["body"].as_str().unwrap_or_default().to_string())
}

fn update_github_review_body(worktree: &Path, review_id: &str, body: &str) -> Result<()> {
    let path = std::env::temp_dir().join(format!(
        "shirei-pr-body-{}-{}.md",
        std::process::id(),
        review_id.replace('/', "-")
    ));
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    let output = Command::new("gh")
        .arg("pr")
        .arg("edit")
        .arg(review_id)
        .arg("--body-file")
        .arg(&path)
        .current_dir(worktree)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .context("failed to run gh pr edit --body-file")?;
    let _ = fs::remove_file(&path);
    if !output.status.success() {
        return Err(anyhow!(
            "gh pr edit --body-file failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn view_github_review(worktree: &Path, selector: &str) -> Result<(Option<String>, Option<String>)> {
    let output = Command::new("gh")
        .args(["pr", "view", selector, "--json", "number,url"])
        .current_dir(worktree)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .context("failed to run gh pr view")?;
    if !output.status.success() {
        return Ok((None, None));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parse gh pr view JSON")?;
    Ok((
        value["number"].as_i64().map(|number| number.to_string()),
        value["url"].as_str().map(str::to_string),
    ))
}

fn preferred_forge(worktree: &Path) -> Result<ForgeKind> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["remote", "-v"])
        .output()
        .context("failed to run git remote -v")?;
    if !output.status.success() {
        return Ok(ForgeKind::Unknown);
    }
    let mut saw_github = false;
    for target in String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(remote_target)
    {
        match target.kind {
            ForgeKind::GitLab => return Ok(ForgeKind::GitLab),
            ForgeKind::GitHub => saw_github = true,
            ForgeKind::Unknown => {}
        }
    }
    if saw_github {
        Ok(ForgeKind::GitHub)
    } else {
        Ok(ForgeKind::Unknown)
    }
}

fn ensure_gitlab_review(request: ReviewRequest<'_>) -> Result<EnsureReviewOutcome> {
    let list = Command::new("glab")
        .args([
            "mr",
            "list",
            "--source-branch",
            request.branch,
            "--all",
            "--output",
            "json",
        ])
        .current_dir(request.worktree)
        .env("GLAB_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .context("failed to run glab mr list")?;
    if !list.status.success() {
        return Err(anyhow!(
            "glab mr list failed: {}",
            String::from_utf8_lossy(&list.stderr).trim()
        ));
    }
    let existing: serde_json::Value =
        serde_json::from_slice(&list.stdout).context("parse glab mr list JSON")?;
    if let Some(item) = existing.as_array().and_then(|items| items.first()) {
        let id = item["iid"]
            .as_i64()
            .or_else(|| item["id"].as_i64())
            .ok_or_else(|| anyhow!("glab mr list returned an item without iid/id"))?
            .to_string();
        let url = item["web_url"]
            .as_str()
            .or_else(|| item["webUrl"].as_str())
            .map(str::to_string);
        let current_base = item["target_branch"]
            .as_str()
            .or_else(|| item["targetBranch"].as_str())
            .unwrap_or_default();
        if current_base != request.base {
            let edit = Command::new("glab")
                .args(["mr", "update", &id, "--target-branch", request.base])
                .current_dir(request.worktree)
                .env("GLAB_PROMPT_DISABLED", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .stdin(Stdio::null())
                .output()
                .context("failed to run glab mr update")?;
            if !edit.status.success() {
                return Err(anyhow!(
                    "glab mr update failed: {}",
                    String::from_utf8_lossy(&edit.stderr).trim()
                ));
            }
            return Ok(EnsureReviewOutcome::Updated { url, id: Some(id) });
        }
        return Ok(EnsureReviewOutcome::AlreadyCorrect { url, id: Some(id) });
    }

    let mut args = vec![
        "mr",
        "create",
        "--source-branch",
        request.branch,
        "--target-branch",
        request.base,
        "--fill",
    ];
    if request.draft {
        args.push("--draft");
    }
    let create = Command::new("glab")
        .args(args)
        .current_dir(request.worktree)
        .env("GLAB_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .context("failed to run glab mr create")?;
    if !create.status.success() {
        return Err(anyhow!(
            "glab mr create failed: {}",
            String::from_utf8_lossy(&create.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&create.stdout).trim().to_string();
    let viewed = view_gitlab_review(request.worktree, request.branch)?;
    Ok(EnsureReviewOutcome::Created {
        url: (!stdout.is_empty()).then_some(stdout).or(viewed.1),
        id: viewed.0,
    })
}

fn get_gitlab_review_body(worktree: &Path, review_id: &str) -> Result<String> {
    let output = Command::new("glab")
        .args(["mr", "view", review_id, "--output", "json"])
        .current_dir(worktree)
        .env("GLAB_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .context("failed to run glab mr view")?;
    if !output.status.success() {
        return Err(anyhow!(
            "glab mr view failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parse glab mr view JSON")?;
    Ok(value["description"]
        .as_str()
        .or_else(|| value["body"].as_str())
        .unwrap_or_default()
        .to_string())
}

fn update_gitlab_review_body(worktree: &Path, review_id: &str, body: &str) -> Result<()> {
    let path = std::env::temp_dir().join(format!(
        "shirei-mr-body-{}-{}.md",
        std::process::id(),
        review_id.replace('/', "-")
    ));
    fs::write(&path, body).with_context(|| format!("write {}", path.display()))?;
    let output = Command::new("glab")
        .arg("mr")
        .arg("update")
        .arg(review_id)
        .arg("--description-file")
        .arg(&path)
        .current_dir(worktree)
        .env("GLAB_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .context("failed to run glab mr update --description-file")?;
    let _ = fs::remove_file(&path);
    if !output.status.success() {
        return Err(anyhow!(
            "glab mr update --description-file failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn view_gitlab_review(worktree: &Path, selector: &str) -> Result<(Option<String>, Option<String>)> {
    let output = Command::new("glab")
        .args(["mr", "view", selector, "--output", "json"])
        .current_dir(worktree)
        .env("GLAB_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .context("failed to run glab mr view")?;
    if !output.status.success() {
        return Ok((None, None));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parse glab mr view JSON")?;
    Ok((
        value["iid"]
            .as_i64()
            .or_else(|| value["id"].as_i64())
            .map(|number| number.to_string()),
        value["web_url"]
            .as_str()
            .or_else(|| value["webUrl"].as_str())
            .map(str::to_string),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_credentials_from_https_remote() {
        let target = remote_target(
            "https://oauth2:secret@gitlab.example.com/group/repo.git?private_token=other-secret#fragment",
        )
        .unwrap();
        assert_eq!(target.display, "gitlab.example.com/group/repo");
        assert_eq!(target.gh_repo, "gitlab.example.com/group/repo");
        assert_eq!(target.glab_repo, "https://gitlab.example.com/group/repo");
    }

    #[test]
    fn strips_user_from_scp_remote() {
        let target = remote_target("git@github.com:owner/repo.git").unwrap();
        assert_eq!(target.display, "github.com/owner/repo");
        assert_eq!(target.gh_repo, "github.com/owner/repo");
        assert_eq!(target.glab_repo, "ssh://git@github.com/owner/repo");
    }
}
