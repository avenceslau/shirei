use anyhow::{anyhow, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

#[derive(Debug)]
pub enum ReviewStatus {
    /// An open review contains the local HEAD.
    Open,
    /// A merged review contains the local HEAD.
    Merged,
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

struct ReviewTarget {
    kind: ForgeKind,
    repository: String,
    display: String,
}

static GH_VERSION_CHECK: OnceLock<std::result::Result<(), String>> = OnceLock::new();
static GLAB_VERSION_CHECK: OnceLock<std::result::Result<(), String>> = OnceLock::new();

fn checked_version(command: &str, minimum: (u32, u32, u32)) -> std::result::Result<(), String> {
    let output = Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("failed to run {command} --version: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{command} --version failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let version_token = text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        if parts.next()? != command {
            return None;
        }
        let value = parts.next()?;
        if value == "version" {
            parts.next()
        } else {
            Some(value)
        }
    });
    let version_token = version_token
        .ok_or_else(|| format!("could not parse {command} version from: {}", text.trim()))?
        .trim_start_matches('v');
    let parse_error = || format!("could not parse {command} version from: {}", text.trim());
    let without_build = match version_token.split_once('+') {
        Some((_, "")) => return Err(parse_error()),
        Some((value, _)) => value,
        None => version_token,
    };
    let (core, prerelease) = match without_build.split_once('-') {
        Some((_, "")) => return Err(parse_error()),
        Some((value, _)) => (value, true),
        None => (without_build, false),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty() || !part.chars().all(|character| character.is_ascii_digit())
        })
    {
        return Err(parse_error());
    }
    let version = (
        parts[0].parse().map_err(|_| parse_error())?,
        parts[1].parse().map_err(|_| parse_error())?,
        parts[2].parse().map_err(|_| parse_error())?,
    );
    if version < minimum || (version == minimum && prerelease) {
        return Err(format!(
            "unsupported {command} version {version_token}; Shirei requires {command} >= {}.{}.{}",
            minimum.0, minimum.1, minimum.2
        ));
    }
    Ok(())
}

fn ensure_gh_version() -> Result<()> {
    GH_VERSION_CHECK
        .get_or_init(|| checked_version("gh", (2, 40, 0)))
        .clone()
        .map_err(|error| anyhow!("{error}; update or reinstall gh"))
}

fn ensure_glab_version() -> Result<()> {
    GLAB_VERSION_CHECK
        .get_or_init(|| checked_version("glab", (1, 50, 0)))
        .clone()
        .map_err(|error| anyhow!("{error}; update or reinstall glab"))
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
            if let Err(error) = ensure_gh_version() {
                errors.push(format!("gh unavailable for {}: {error}", target.display));
            } else {
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
                                        return if merged {
                                            ReviewStatus::Merged
                                        } else {
                                            ReviewStatus::Open
                                        };
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
        }

        if !matches!(target.kind, ForgeKind::GitHub) {
            if let Err(error) = ensure_glab_version() {
                errors.push(format!("glab unavailable for {}: {error}", target.display));
            } else {
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
                    .env("GLAB_NO_PROMPT", "1")
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
                                        return if merged {
                                            ReviewStatus::Merged
                                        } else {
                                            ReviewStatus::Open
                                        };
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
    pub remote: &'a str,
    pub title: Option<&'a str>,
    pub body: Option<&'a str>,
    pub draft: bool,
}

#[derive(Debug, Clone)]
pub enum EnsureReviewOutcome {
    Created {
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
            | EnsureReviewOutcome::AlreadyCorrect { id, .. } => id.as_deref(),
        }
    }

    pub fn url(&self) -> Option<&str> {
        match self {
            EnsureReviewOutcome::Created { url, .. }
            | EnsureReviewOutcome::AlreadyCorrect { url, .. } => url.as_deref(),
        }
    }
}

pub fn review_url_identity(url: &str) -> Result<String> {
    remote_target(url)
        .map(|target| target.display)
        .ok_or_else(|| anyhow!("unsupported forge remote format"))
}

pub fn review_remote_identity(worktree: &Path, remote: &str) -> Result<String> {
    Ok(review_target(worktree, remote)?.display)
}

pub fn validate_review_cli(worktree: &Path, remote: &str) -> Result<()> {
    match review_target(worktree, remote)?.kind {
        ForgeKind::GitHub => ensure_gh_version(),
        ForgeKind::GitLab => ensure_glab_version(),
        ForgeKind::Unknown => unreachable!(),
    }
}

#[derive(Debug, Clone)]
pub struct FoundReview {
    pub id: String,
    pub url: Option<String>,
}

/// The open review of `branch` targeting `base`, without creating one.
pub fn find_open_review(
    worktree: &Path,
    remote: &str,
    branch: &str,
    base: &str,
) -> Result<Option<FoundReview>> {
    let target = review_target(worktree, remote)?;
    match target.kind {
        ForgeKind::GitHub => {
            ensure_gh_version()?;
            find_github_review(worktree, branch, base, &target.repository)
        }
        ForgeKind::GitLab => {
            ensure_glab_version()?;
            find_gitlab_review(worktree, branch, base, &target.repository)
        }
        ForgeKind::Unknown => unreachable!(),
    }
}

pub fn ensure_review(request: ReviewRequest<'_>) -> Result<EnsureReviewOutcome> {
    let target = review_target(request.worktree, request.remote)?;
    match target.kind {
        ForgeKind::GitHub => {
            ensure_gh_version()?;
            ensure_github_review(request, &target.repository)
        }
        ForgeKind::GitLab => {
            ensure_glab_version()?;
            ensure_gitlab_review(request, &target.repository)
        }
        ForgeKind::Unknown => unreachable!(),
    }
}

pub fn get_review_body(worktree: &Path, remote: &str, review_id: &str) -> Result<String> {
    let target = review_target(worktree, remote)?;
    match target.kind {
        ForgeKind::GitHub => {
            ensure_gh_version()?;
            get_github_review_body(worktree, review_id, &target.repository)
        }
        ForgeKind::GitLab => {
            ensure_glab_version()?;
            get_gitlab_review_body(worktree, review_id, &target.repository)
        }
        ForgeKind::Unknown => unreachable!(),
    }
}

pub fn update_review_body(
    worktree: &Path,
    remote: &str,
    review_id: &str,
    body: &str,
) -> Result<()> {
    let target = review_target(worktree, remote)?;
    match target.kind {
        ForgeKind::GitHub => {
            ensure_gh_version()?;
            update_github_review_body(worktree, review_id, body, &target.repository)
        }
        ForgeKind::GitLab => {
            ensure_glab_version()?;
            update_gitlab_review_body(worktree, review_id, body, &target.repository)
        }
        ForgeKind::Unknown => unreachable!(),
    }
}

fn find_github_review(
    worktree: &Path,
    branch: &str,
    base: &str,
    repository: &str,
) -> Result<Option<FoundReview>> {
    let list = Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--base",
            base,
            "--state",
            "open",
            "--limit",
            "100",
            "--json",
            "number,url,baseRefName,headRepository,headRepositoryOwner",
            "--repo",
            repository,
        ])
        .current_dir(worktree)
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
    let items = existing
        .as_array()
        .ok_or_else(|| anyhow!("gh pr list returned non-array JSON"))?;
    let mut repository_parts = repository.rsplit('/');
    let repository_name = repository_parts
        .next()
        .ok_or_else(|| anyhow!("invalid GitHub repository target"))?;
    let repository_owner = repository_parts
        .next()
        .ok_or_else(|| anyhow!("invalid GitHub repository target"))?;
    let repository_with_owner = format!("{repository_owner}/{repository_name}");
    let candidates: Vec<&serde_json::Value> = items
        .iter()
        .filter(|item| {
            let owner = item["headRepositoryOwner"]["login"].as_str();
            let name = item["headRepository"]["name"].as_str();
            let name_with_owner = item["headRepository"]["nameWithOwner"].as_str();
            (owner == Some(repository_owner) && name == Some(repository_name))
                || name_with_owner == Some(repository_with_owner.as_str())
        })
        .collect();
    let exact = candidates
        .iter()
        .copied()
        .find(|item| item["baseRefName"].as_str() == Some(base));
    if exact.is_none() && candidates.len() > 1 {
        return Err(anyhow!(
            "multiple open GitHub pull requests exist for {branch}; specify the intended base outside Shirei"
        ));
    }
    let Some(item) = exact.or_else(|| candidates.first().copied()) else {
        return Ok(None);
    };
    let number = item["number"]
        .as_i64()
        .ok_or_else(|| anyhow!("gh pr list returned an item without number"))?
        .to_string();
    Ok(Some(FoundReview {
        id: number,
        url: item["url"].as_str().map(str::to_string),
    }))
}

fn ensure_github_review(
    request: ReviewRequest<'_>,
    repository: &str,
) -> Result<EnsureReviewOutcome> {
    let existing = find_github_review(request.worktree, request.branch, request.base, repository)?;
    if let Some(found) = existing {
        return Ok(EnsureReviewOutcome::AlreadyCorrect {
            url: found.url,
            id: Some(found.id),
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
        "--repo",
        repository,
    ];
    if let Some(title) = request.title {
        args.extend(["--title", title]);
    }
    if let Some(body) = request.body {
        args.extend(["--body", body]);
    }
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
    let output = String::from_utf8_lossy(&create.stdout);
    let created_url = output
        .split_whitespace()
        .rev()
        .find(|value| value.starts_with("http://") || value.starts_with("https://"))
        .map(str::to_string);
    let selector = created_url.as_deref().unwrap_or(request.branch);
    let viewed = view_github_review(request.worktree, selector, repository)?;
    Ok(EnsureReviewOutcome::Created {
        url: created_url.or(viewed.1),
        id: viewed.0,
    })
}

fn get_github_review_body(worktree: &Path, review_id: &str, repository: &str) -> Result<String> {
    let output = Command::new("gh")
        .args([
            "pr", "view", review_id, "--json", "body", "--repo", repository,
        ])
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

fn update_github_review_body(
    worktree: &Path,
    review_id: &str,
    body: &str,
    repository: &str,
) -> Result<()> {
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
        .arg("--repo")
        .arg(repository)
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

fn view_github_review(
    worktree: &Path,
    selector: &str,
    repository: &str,
) -> Result<(Option<String>, Option<String>)> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            selector,
            "--json",
            "number,url",
            "--repo",
            repository,
        ])
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

fn review_target(worktree: &Path, remote: &str) -> Result<ReviewTarget> {
    let url = crate::git::remote_url(worktree, remote)?;
    let target = remote_target(&url)
        .ok_or_else(|| anyhow!("unsupported forge remote format for {remote}"))?;
    let kind = match target.kind {
        ForgeKind::GitHub => ForgeKind::GitHub,
        ForgeKind::GitLab => ForgeKind::GitLab,
        ForgeKind::Unknown => ForgeKind::GitHub,
    };
    let repository = match kind {
        ForgeKind::GitHub => target.gh_repo,
        ForgeKind::GitLab => target.glab_repo,
        ForgeKind::Unknown => unreachable!(),
    };
    Ok(ReviewTarget {
        kind,
        repository,
        display: target.display,
    })
}

fn find_gitlab_review(
    worktree: &Path,
    branch: &str,
    base: &str,
    repository: &str,
) -> Result<Option<FoundReview>> {
    let list = Command::new("glab")
        .args([
            "mr",
            "list",
            "--source-branch",
            branch,
            "--target-branch",
            base,
            "--per-page",
            "100",
            "--output",
            "json",
            "--repo",
            repository,
        ])
        .current_dir(worktree)
        .env("GLAB_NO_PROMPT", "1")
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
    let items = existing
        .as_array()
        .ok_or_else(|| anyhow!("glab mr list returned non-array JSON"))?;
    let candidates: Vec<&serde_json::Value> = items
        .iter()
        .filter(|item| {
            let source_project = item["source_project_id"]
                .as_i64()
                .or_else(|| item["sourceProjectId"].as_i64());
            let target_project = item["target_project_id"]
                .as_i64()
                .or_else(|| item["targetProjectId"].as_i64());
            source_project.is_some() && source_project == target_project
        })
        .collect();
    let exact = candidates.iter().copied().find(|item| {
        item["target_branch"]
            .as_str()
            .or_else(|| item["targetBranch"].as_str())
            == Some(base)
    });
    if exact.is_none() && candidates.len() > 1 {
        return Err(anyhow!(
            "multiple open GitLab merge requests exist for {branch}; specify the intended base outside Shirei"
        ));
    }
    let Some(item) = exact.or_else(|| candidates.first().copied()) else {
        return Ok(None);
    };
    let id = item["iid"]
        .as_i64()
        .or_else(|| item["id"].as_i64())
        .ok_or_else(|| anyhow!("glab mr list returned an item without iid/id"))?
        .to_string();
    Ok(Some(FoundReview {
        id,
        url: item["web_url"]
            .as_str()
            .or_else(|| item["webUrl"].as_str())
            .map(str::to_string),
    }))
}

fn ensure_gitlab_review(
    request: ReviewRequest<'_>,
    repository: &str,
) -> Result<EnsureReviewOutcome> {
    let existing = find_gitlab_review(request.worktree, request.branch, request.base, repository)?;
    if let Some(found) = existing {
        return Ok(EnsureReviewOutcome::AlreadyCorrect {
            url: found.url,
            id: Some(found.id),
        });
    }

    let generated_title = if request.title.is_none() {
        let output = Command::new("git")
            .arg("-C")
            .arg(request.worktree)
            .args(["log", "-1", "--format=%s", request.branch])
            .output()
            .context("failed to derive merge request title from git")?;
        if !output.status.success() {
            return Err(anyhow!(
                "git log failed while deriving merge request title: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    };
    let generated_body = if request.body.is_none() {
        let output = Command::new("git")
            .arg("-C")
            .arg(request.worktree)
            .args(["log", "-1", "--format=%b", request.branch])
            .output()
            .context("failed to derive merge request description from git")?;
        if !output.status.success() {
            return Err(anyhow!(
                "git log failed while deriving merge request description: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    };
    let title = request
        .title
        .or(generated_title.as_deref())
        .unwrap_or_default();
    let body = request
        .body
        .or(generated_body.as_deref())
        .unwrap_or_default();
    let mut args = vec![
        "mr",
        "create",
        "--source-branch",
        request.branch,
        "--target-branch",
        request.base,
        "--title",
        title,
        "--description",
        body,
        "--yes",
        "--repo",
        repository,
        // Name the source project instead of letting glab infer it from git
        // remotes; split-source reviews are not supported.
        "--head",
        repository,
    ];
    if request.draft {
        args.push("--draft");
    }
    let create = Command::new("glab")
        .args(args)
        .current_dir(request.worktree)
        .env("GLAB_NO_PROMPT", "1")
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
    let output = String::from_utf8_lossy(&create.stdout);
    let created_url = output
        .split_whitespace()
        .rev()
        .find(|value| value.starts_with("http://") || value.starts_with("https://"))
        .map(str::to_string);
    let created_id = created_url.as_deref().and_then(|url| {
        url.trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|value| value.chars().all(|character| character.is_ascii_digit()))
    });
    let selector = created_id.unwrap_or(request.branch);
    let viewed = view_gitlab_review(request.worktree, selector, repository)?;
    Ok(EnsureReviewOutcome::Created {
        url: created_url.or(viewed.1),
        id: viewed.0,
    })
}

fn get_gitlab_review_body(worktree: &Path, review_id: &str, repository: &str) -> Result<String> {
    let output = Command::new("glab")
        .args([
            "mr", "view", review_id, "--output", "json", "--repo", repository,
        ])
        .current_dir(worktree)
        .env("GLAB_NO_PROMPT", "1")
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

fn update_gitlab_review_body(
    worktree: &Path,
    review_id: &str,
    body: &str,
    repository: &str,
) -> Result<()> {
    // glab has no --description-file. The body goes in one `--description=`
    // argument so a body that starts with `-` is not parsed as a flag.
    let output = Command::new("glab")
        .arg("mr")
        .arg("update")
        .arg(review_id)
        .arg(format!("--description={body}"))
        .arg("--repo")
        .arg(repository)
        .current_dir(worktree)
        .env("GLAB_NO_PROMPT", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .context("failed to run glab mr update --description")?;
    if !output.status.success() {
        return Err(anyhow!(
            "glab mr update --description failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn view_gitlab_review(
    worktree: &Path,
    selector: &str,
    repository: &str,
) -> Result<(Option<String>, Option<String>)> {
    let output = Command::new("glab")
        .args([
            "mr", "view", selector, "--output", "json", "--repo", repository,
        ])
        .current_dir(worktree)
        .env("GLAB_NO_PROMPT", "1")
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
