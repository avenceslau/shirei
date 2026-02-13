use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy)]
pub enum VcsKind {
    GitHub,
    GitLab,
    Mock,
}

#[derive(Debug, Clone)]
pub struct RepoId {
    pub owner: String, // for gitlab this may include subgroups
    pub name: String,
}

impl RepoId {
    pub fn parse_owner_repo(s: &str) -> Option<Self> {
        let (owner, name) = s.rsplit_once('/')?;
        Some(Self {
            owner: owner.to_string(),
            name: name.to_string(),
        })
    }
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone)]
pub struct PullRequestId {
    pub repo: RepoId,
    pub number: u64, // PR number or MR iid
}

#[derive(Debug, Clone)]
pub struct CreatePrRequest {
    pub title: String,
    pub body: String,
    pub head_branch: String,
    pub base_branch: String,
    pub draft: bool,
}

#[derive(Debug, Clone)]
pub struct CreatedPr {
    pub number: u64,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrInfo {
    pub merged: bool,
    pub merged_at: Option<String>,
    pub state: Option<String>,
    pub head_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrComment {
    pub id: u64,
    pub author: String,
    pub body: String,
    pub created_at: Option<String>,
}

#[derive(thiserror::Error, Debug)]
pub enum VcsError {
    #[error("auth: {0}")]
    Auth(String),
    #[error("not found")]
    NotFound,
    #[error("rate limited")]
    RateLimited,
    #[error("provider: {0}")]
    Provider(String),
}

pub fn parse_repo_key(repo_key: &str) -> Option<(VcsKind, RepoId)> {
    let (prefix, rest) = repo_key.split_once(':')?;
    let kind = match prefix {
        "github" => VcsKind::GitHub,
        "gitlab" => VcsKind::GitLab,
        "mock" => VcsKind::Mock,
        _ => return None,
    };
    let repo = RepoId::parse_owner_repo(rest)?;
    Some((kind, repo))
}

#[async_trait]
pub trait VcsProvider: Send + Sync {
    fn kind(&self) -> &'static str;
    async fn get_pr(&self, pr: &PullRequestId) -> Result<PrInfo, VcsError>;
    async fn list_comments(&self, pr: &PullRequestId) -> Result<Vec<PrComment>, VcsError>;
    async fn create_comment(&self, pr: &PullRequestId, body: &str) -> Result<(), VcsError>;
    async fn create_pr(
        &self,
        pr: &PullRequestId,
        req: &CreatePrRequest,
    ) -> Result<CreatedPr, VcsError>;
}

pub fn repo_key_from_remote(remote: &str) -> Option<String> {
    let raw = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    if raw.is_empty() {
        return None;
    }

    let (host, path) = if let Some(rest) = raw.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        (host.to_ascii_lowercase(), path.to_string())
    } else if raw.contains("://") {
        let url = url::Url::parse(raw).ok()?;
        (
            url.host_str()?.to_ascii_lowercase(),
            url.path().trim_start_matches('/').to_string(),
        )
    } else {
        return None;
    };

    if path.is_empty() || !path.contains('/') {
        return None;
    }

    if host.contains("github") {
        return Some(format!("github:{path}"));
    }
    if host.contains("gitlab") {
        return Some(format!("gitlab:{path}"));
    }

    None
}

pub fn pr_web_url(repo_key: &str, pr_number: u64) -> Option<String> {
    let (kind, repo) = parse_repo_key(repo_key)?;
    match kind {
        VcsKind::GitHub => Some(format!(
            "https://github.com/{}/{}/pull/{}",
            repo.owner, repo.name, pr_number
        )),
        VcsKind::GitLab => Some(format!(
            "https://gitlab.com/{}/{}/-/merge_requests/{}",
            repo.owner, repo.name, pr_number
        )),
        VcsKind::Mock => None,
    }
}

pub mod factory;
pub mod github;
pub mod gitlab;
pub mod mock;

#[cfg(test)]
mod tests {
    use super::{pr_web_url, repo_key_from_remote};

    #[test]
    fn parses_repo_key_from_https_remote() {
        assert_eq!(
            repo_key_from_remote("https://github.com/acme/demo.git"),
            Some("github:acme/demo".to_string())
        );
    }

    #[test]
    fn parses_repo_key_from_ssh_remote() {
        assert_eq!(
            repo_key_from_remote("git@gitlab.com:group/sub/repo.git"),
            Some("gitlab:group/sub/repo".to_string())
        );
    }

    #[test]
    fn builds_github_pr_web_url() {
        assert_eq!(
            pr_web_url("github:acme/demo", 42),
            Some("https://github.com/acme/demo/pull/42".to_string())
        );
    }
}
