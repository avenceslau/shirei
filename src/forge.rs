use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug)]
pub enum ReviewStatus {
    Covered,
    Stale,
    NoneFound,
    Unknown(Vec<String>),
}

#[derive(Clone, Copy)]
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
