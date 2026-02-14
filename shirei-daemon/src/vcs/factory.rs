use std::sync::Arc;

use anyhow::Result;

use super::github::GitHubProvider;
use super::gitlab::GitLabProvider;
use super::mock::MockProvider;
use super::{VcsKind, VcsProvider};

#[derive(Clone)]
pub struct ProviderFactory {
    github: Option<Arc<dyn VcsProvider>>,
    gitlab: Option<Arc<dyn VcsProvider>>,
    mock: Arc<dyn VcsProvider>,
}

impl ProviderFactory {
    pub async fn from_env() -> Result<Self> {
        let github = std::env::var("SHIREI_GITHUB_TOKEN")
            .ok()
            .map(|t| Arc::new(GitHubProvider::new(t).unwrap()) as Arc<dyn VcsProvider>);
        let gitlab = match (
            std::env::var("SHIREI_GITLAB_HOST").ok(),
            std::env::var("SHIREI_GITLAB_TOKEN").ok(),
        ) {
            (Some(host), Some(token)) => {
                Some(Arc::new(GitLabProvider::new(host, token).await?) as Arc<dyn VcsProvider>)
            }
            _ => None,
        };
        Ok(Self {
            github,
            gitlab,
            mock: Arc::new(MockProvider::default()),
        })
    }

    pub fn get(&self, kind: VcsKind) -> Arc<dyn VcsProvider> {
        match kind {
            VcsKind::GitHub => self
                .github
                .as_ref()
                .expect("GitHub provider not configured")
                .clone(),
            VcsKind::GitLab => self
                .gitlab
                .as_ref()
                .expect("GitLab provider not configured")
                .clone(),
            VcsKind::Mock => self.mock.clone(),
        }
    }
}
