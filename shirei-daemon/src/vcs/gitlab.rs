use super::*;
use async_trait::async_trait;
use serde_json::Value;

pub struct GitLabProvider {
    host: String,
    token: String,
    client: reqwest::Client,
}

impl GitLabProvider {
    pub async fn new(host: String, token: String) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder().build()?;
        Ok(Self {
            host,
            token,
            client,
        })
    }

    fn project_path(repo: &RepoId) -> String {
        format!("{}/{}", repo.owner, repo.name)
    }

    fn enc_project(project: &str) -> String {
        project.replace('/', "%2F")
    }

    fn api_base(&self) -> String {
        format!("https://{}/api/v4", self.host.trim_end_matches('/'))
    }
}

#[async_trait]
impl VcsProvider for GitLabProvider {
    fn kind(&self) -> &'static str {
        "gitlab"
    }

    async fn get_pr(&self, pr: &PullRequestId) -> Result<PrInfo, VcsError> {
        let project = Self::enc_project(&Self::project_path(&pr.repo));
        let url = format!(
            "{}/projects/{project}/merge_requests/{}",
            self.api_base(),
            pr.number
        );
        let v: Value = self
            .client
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| VcsError::Provider(e.to_string()))?
            .error_for_status()
            .map_err(|e| VcsError::Provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| VcsError::Provider(e.to_string()))?;

        let state = v
            .get("state")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        let merged = matches!(state.as_deref(), Some("merged"));
        let merged_at = v
            .get("merged_at")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        let head_sha = v.get("sha").and_then(|x| x.as_str()).map(|s| s.to_string());
        Ok(PrInfo {
            merged,
            merged_at,
            state,
            head_sha,
        })
    }

    async fn list_comments(&self, pr: &PullRequestId) -> Result<Vec<PrComment>, VcsError> {
        let project = Self::enc_project(&Self::project_path(&pr.repo));
        let url = format!(
            "{}/projects/{project}/merge_requests/{}/notes",
            self.api_base(),
            pr.number
        );
        let notes: Vec<Value> = self
            .client
            .get(url)
            .bearer_auth(&self.token)
            .query(&[("per_page", "100")])
            .send()
            .await
            .map_err(|e| VcsError::Provider(e.to_string()))?
            .error_for_status()
            .map_err(|e| VcsError::Provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| VcsError::Provider(e.to_string()))?;

        let mut out = Vec::new();
        for n in notes {
            out.push(PrComment {
                id: n.get("id").and_then(|x| x.as_u64()).unwrap_or(0),
                author: n
                    .get("author")
                    .and_then(|a| a.get("username"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                body: n
                    .get("body")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                created_at: n
                    .get("created_at")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string()),
            });
        }
        Ok(out)
    }

    async fn create_comment(&self, pr: &PullRequestId, body: &str) -> Result<(), VcsError> {
        let project = Self::enc_project(&Self::project_path(&pr.repo));
        let url = format!(
            "{}/projects/{project}/merge_requests/{}/notes",
            self.api_base(),
            pr.number
        );
        self.client
            .post(url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .map_err(|e| VcsError::Provider(e.to_string()))?
            .error_for_status()
            .map_err(|e| VcsError::Provider(e.to_string()))?;
        Ok(())
    }

    async fn create_pr(
        &self,
        pr: &PullRequestId,
        req: &CreatePrRequest,
    ) -> Result<CreatedPr, VcsError> {
        let project = Self::enc_project(&Self::project_path(&pr.repo));
        let url = format!("{}/projects/{project}/merge_requests", self.api_base());
        let v: Value = self
            .client
            .post(url)
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "title": req.title,
                "description": req.body,
                "source_branch": req.head_branch,
                "target_branch": req.base_branch,
                "draft": req.draft,
            }))
            .send()
            .await
            .map_err(|e| VcsError::Provider(e.to_string()))?
            .error_for_status()
            .map_err(|e| VcsError::Provider(e.to_string()))?
            .json()
            .await
            .map_err(|e| VcsError::Provider(e.to_string()))?;

        let number = v.get("iid").and_then(|x| x.as_u64()).ok_or_else(|| {
            VcsError::Provider("gitlab create_pr missing iid in response".to_string())
        })?;
        let web = v
            .get("web_url")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());

        Ok(CreatedPr { number, url: web })
    }
}
