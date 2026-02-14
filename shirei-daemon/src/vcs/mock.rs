use super::*;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct MockProvider {
    pub pr_info: Arc<Mutex<Option<PrInfo>>>,
    pub comments: Arc<Mutex<Vec<PrComment>>>,
    pub posted: Arc<Mutex<Vec<String>>>,
    pub created_prs: Arc<Mutex<Vec<CreatePrRequest>>>,
}

#[async_trait]
impl VcsProvider for MockProvider {
    fn kind(&self) -> &'static str {
        "mock"
    }

    async fn get_pr(&self, _pr: &PullRequestId) -> Result<PrInfo, VcsError> {
        self.pr_info
            .lock()
            .unwrap()
            .clone()
            .ok_or(VcsError::NotFound)
    }

    async fn list_comments(&self, _pr: &PullRequestId) -> Result<Vec<PrComment>, VcsError> {
        Ok(self.comments.lock().unwrap().clone())
    }

    async fn create_comment(&self, _pr: &PullRequestId, body: &str) -> Result<(), VcsError> {
        self.posted.lock().unwrap().push(body.to_string());
        Ok(())
    }

    async fn create_pr(
        &self,
        _pr: &PullRequestId,
        req: &CreatePrRequest,
    ) -> Result<CreatedPr, VcsError> {
        self.created_prs.lock().unwrap().push(req.clone());
        Ok(CreatedPr {
            number: 1,
            url: Some("https://mock.local/pr/1".to_string()),
        })
    }
}
