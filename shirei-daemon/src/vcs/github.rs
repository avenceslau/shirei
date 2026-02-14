use super::*;
use async_trait::async_trait;
use octocrab::Octocrab;

pub struct GitHubProvider {
    crab: Octocrab,
}

impl GitHubProvider {
    pub fn new(token: String) -> anyhow::Result<Self> {
        let crab = Octocrab::builder().personal_token(token).build()?;
        Ok(Self { crab })
    }
}

#[async_trait]
impl VcsProvider for GitHubProvider {
    fn kind(&self) -> &'static str {
        "github"
    }

    async fn get_pr(&self, pr: &PullRequestId) -> Result<PrInfo, VcsError> {
        let pull = self
            .crab
            .pulls(&pr.repo.owner, &pr.repo.name)
            .get(pr.number)
            .await
            .map_err(|e| VcsError::Provider(e.to_string()))?;

        Ok(PrInfo {
            merged: pull.merged_at.is_some(),
            merged_at: pull.merged_at.map(|t| t.to_rfc3339()),
            state: pull.state.map(|s| format!("{:?}", s)),
            head_sha: Some(pull.head.sha),
        })
    }

    async fn list_comments(&self, pr: &PullRequestId) -> Result<Vec<PrComment>, VcsError> {
        let page = self
            .crab
            .issues(&pr.repo.owner, &pr.repo.name)
            .list_comments(pr.number)
            .send()
            .await
            .map_err(|e| VcsError::Provider(e.to_string()))?;

        Ok(page
            .items
            .into_iter()
            .map(|c| PrComment {
                id: c.id.0,
                author: c.user.login,
                body: c.body.unwrap_or_default(),
                created_at: Some(c.created_at.to_rfc3339()),
            })
            .collect())
    }

    async fn create_comment(&self, pr: &PullRequestId, body: &str) -> Result<(), VcsError> {
        self.crab
            .issues(&pr.repo.owner, &pr.repo.name)
            .create_comment(pr.number, body)
            .await
            .map_err(|e| VcsError::Provider(e.to_string()))?;
        Ok(())
    }

    async fn create_pr(
        &self,
        pr: &PullRequestId,
        req: &CreatePrRequest,
    ) -> Result<CreatedPr, VcsError> {
        let created = self
            .crab
            .pulls(&pr.repo.owner, &pr.repo.name)
            .create(&req.title, &req.head_branch, &req.base_branch)
            .body(&req.body)
            .draft(req.draft)
            .send()
            .await
            .map_err(|e| VcsError::Provider(e.to_string()))?;

        Ok(CreatedPr {
            number: created.number,
            url: created.html_url.map(|u| u.to_string()),
        })
    }
}
