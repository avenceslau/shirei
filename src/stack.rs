use anyhow::{anyhow, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::state::{State, Worktree};

#[derive(Debug, Clone)]
pub struct StackNode {
    pub branch: String,
    pub base: String,
    pub depth: usize,
    pub worktree: Option<Worktree>,
    pub review_url: Option<String>,
    pub problems: Vec<StackProblem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StackProblem {
    MissingWorktree,
    MissingBaseMetadata(String),
}

impl StackProblem {
    fn label(&self) -> String {
        match self {
            StackProblem::MissingWorktree => "missing-worktree".to_string(),
            StackProblem::MissingBaseMetadata(base) => format!("missing-base-metadata:{base}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct StackGraph {
    pub repo: PathBuf,
    pub nodes: Vec<StackNode>,
}

#[derive(Debug, Clone)]
pub struct RebasePlan {
    pub repo: PathBuf,
    pub leaf: String,
    pub steps: Vec<RebaseStep>,
}

#[derive(Debug, Clone)]
pub struct RebaseStep {
    pub branch: String,
    pub base: String,
    pub worktree: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ConflictReport {
    pub branch: String,
    pub base: String,
    pub worktree: PathBuf,
    pub files: Vec<ConflictFile>,
}

#[derive(Debug, Clone)]
pub struct ConflictFile {
    pub path: PathBuf,
    pub blocks: Vec<ConflictBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictBlock {
    pub start_line: usize,
    pub separator_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone)]
pub struct SubmitPlan {
    pub repo: PathBuf,
    pub steps: Vec<SubmitStep>,
}

#[derive(Debug, Clone)]
pub struct SubmitStep {
    pub branch: String,
    pub base: String,
    pub worktree: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct SubmittedReview {
    pub branch: String,
    pub base: String,
    pub id: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackLinks {
    pub previous: Option<ReviewLink>,
    pub next: Vec<ReviewLink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewLink {
    pub branch: String,
    pub url: String,
}

pub fn build_graph(state: &State, repo: &Path) -> Result<StackGraph> {
    let mut nodes = Vec::new();
    let by_branch = stack_by_branch(state, repo);
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for branch in by_branch.keys() {
        visit_graph_node(
            state,
            repo,
            branch,
            &by_branch,
            &mut visiting,
            &mut visited,
            &mut nodes,
        )?;
    }
    nodes.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| left.branch.cmp(&right.branch))
    });
    Ok(StackGraph {
        repo: repo.to_path_buf(),
        nodes,
    })
}

pub fn validate_edge(state: &State, repo: &Path, branch: &str, base: &str) -> Result<()> {
    if branch == base {
        return Err(anyhow!("stack branch cannot use itself as base: {branch}"));
    }
    let mut by_branch: BTreeMap<String, String> = state
        .stack_branches
        .iter()
        .filter(|item| path_eq(&item.repo, repo) && item.branch != branch)
        .map(|item| (item.branch.clone(), item.base.clone()))
        .collect();
    by_branch.insert(branch.to_string(), base.to_string());
    for candidate in by_branch.keys() {
        let mut seen = BTreeSet::new();
        let mut current = candidate.as_str();
        while let Some(next) = by_branch.get(current) {
            if !seen.insert(current.to_string()) {
                return Err(anyhow!("stack metadata would contain a cycle at {current}"));
            }
            current = next;
        }
    }
    Ok(())
}

pub fn plan_rebase(
    state: &State,
    repo: &Path,
    leaf: Option<&str>,
    onto: Option<&str>,
) -> Result<RebasePlan> {
    let leaf = resolve_leaf(state, repo, leaf)?;
    let branches = ancestry(state, repo, &leaf)?;
    let mut steps = Vec::new();
    for branch in branches {
        let item = state
            .stack_branch(repo, &branch)
            .ok_or_else(|| anyhow!("no stack metadata for {branch}"))?;
        let worktree = state
            .worktree_for_branch(repo, &branch)
            .ok_or_else(|| anyhow!("no tracked worktree for stack branch {branch}"))?;
        steps.push(RebaseStep {
            branch,
            base: onto
                .filter(|_| steps.is_empty())
                .unwrap_or(&item.base)
                .to_string(),
            worktree: worktree.path.clone(),
        });
    }
    Ok(RebasePlan {
        repo: repo.to_path_buf(),
        leaf,
        steps,
    })
}

pub fn find_conflicts(worktree: &Path, branch: &str, base: &str) -> Result<ConflictReport> {
    let files = crate::git::unmerged_files(worktree)?
        .into_iter()
        .map(|path| {
            let blocks = conflict_blocks(&worktree.join(&path)).unwrap_or_default();
            ConflictFile { path, blocks }
        })
        .collect();
    Ok(ConflictReport {
        branch: branch.to_string(),
        base: base.to_string(),
        worktree: worktree.to_path_buf(),
        files,
    })
}

pub fn format_conflict_report(report: &ConflictReport) -> Vec<String> {
    let mut lines = vec![format!(
        "status=conflict branch={} base={} worktree={}",
        report.branch,
        report.base,
        report.worktree.display()
    )];
    for file in &report.files {
        let blocks = if file.blocks.is_empty() {
            "unknown".to_string()
        } else {
            file.blocks
                .iter()
                .map(|block| format!("{}-{}", block.start_line, block.end_line))
                .collect::<Vec<_>>()
                .join(",")
        };
        lines.push(format!(
            "conflict file={} blocks={blocks}",
            file.path.display()
        ));
    }
    lines.push(
        "next=resolve conflicts, git add files, then run `shirei stack rebase --continue`"
            .to_string(),
    );
    lines.push("abort=`shirei stack rebase --abort`".to_string());
    lines
}

pub fn plan_submit(state: &State, repo: &Path, leaf: Option<&str>) -> Result<SubmitPlan> {
    let leaf = resolve_leaf(state, repo, leaf)?;
    let branches = ancestry(state, repo, &leaf)?;
    let steps = branches
        .into_iter()
        .map(|branch| {
            let item = state
                .stack_branch(repo, &branch)
                .ok_or_else(|| anyhow!("no stack metadata for {branch}"))?;
            Ok(SubmitStep {
                branch,
                base: item.base.clone(),
                worktree: state
                    .worktree_for_branch(repo, &item.branch)
                    .map(|worktree| worktree.path.clone()),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(SubmitPlan {
        repo: repo.to_path_buf(),
        steps,
    })
}

pub fn build_stack_links(reviews: &[SubmittedReview]) -> Result<BTreeMap<String, StackLinks>> {
    let by_branch: BTreeMap<_, _> = reviews
        .iter()
        .map(|review| (review.branch.clone(), review))
        .collect();
    let mut links: BTreeMap<String, StackLinks> = BTreeMap::new();
    for review in reviews {
        let previous = by_branch
            .get(&review.base)
            .map(|parent| {
                parent
                    .url
                    .as_ref()
                    .map(|url| ReviewLink {
                        branch: parent.branch.clone(),
                        url: url.clone(),
                    })
                    .ok_or_else(|| {
                        anyhow!("missing review URL for parent branch {}", parent.branch)
                    })
            })
            .transpose()?;
        let mut next = Vec::new();
        for child in reviews
            .iter()
            .filter(|candidate| candidate.base == review.branch)
        {
            let url = child
                .url
                .as_ref()
                .ok_or_else(|| anyhow!("missing review URL for child branch {}", child.branch))?;
            next.push(ReviewLink {
                branch: child.branch.clone(),
                url: url.clone(),
            });
        }
        links.insert(review.branch.clone(), StackLinks { previous, next });
    }
    Ok(links)
}

pub fn render_stack_links(links: &StackLinks) -> String {
    let mut out = Vec::new();
    out.push("<!-- shirei-stack-links:start -->".to_string());
    out.push("Stack navigation:".to_string());
    match &links.previous {
        Some(previous) => out.push(format!(
            "- Previous: [{}]({})",
            previous.branch, previous.url
        )),
        None => out.push("- Previous: none".to_string()),
    }
    match links.next.as_slice() {
        [] => out.push("- Next: none".to_string()),
        [single] => out.push(format!("- Next: [{}]({})", single.branch, single.url)),
        many => {
            out.push("- Next:".to_string());
            for next in many {
                out.push(format!("  - [{}]({})", next.branch, next.url));
            }
        }
    }
    out.push("<!-- shirei-stack-links:end -->".to_string());
    out.join("\n")
}

pub fn replace_stack_links_block(existing_body: &str, rendered_block: &str) -> Result<String> {
    const START: &str = "<!-- shirei-stack-links:start -->";
    const END: &str = "<!-- shirei-stack-links:end -->";
    let starts: Vec<_> = existing_body.match_indices(START).collect();
    let ends: Vec<_> = existing_body.match_indices(END).collect();
    match (starts.len(), ends.len()) {
        (0, 0) => {
            if existing_body.is_empty() {
                Ok(rendered_block.to_string())
            } else {
                Ok(format!("{}\n\n{}", existing_body, rendered_block))
            }
        }
        (1, 1) => {
            let start = starts[0].0;
            let end = ends[0].0 + END.len();
            if starts[0].0 > ends[0].0 {
                return Err(anyhow!("malformed Shirei stack-links block markers"));
            }
            Ok(format!(
                "{}{}{}",
                &existing_body[..start],
                rendered_block,
                &existing_body[end..]
            ))
        }
        _ => Err(anyhow!(
            "malformed or duplicated Shirei stack-links block markers"
        )),
    }
}

pub fn format_list(graph: &StackGraph, porcelain: bool) -> Vec<String> {
    let mut lines = Vec::new();
    if porcelain {
        lines.push(format!("repo\t{}", graph.repo.display()));
        for node in &graph.nodes {
            lines.push(format!(
                "branch\t{}\t{}\tbase={}\tpath={}\treview={}\tproblems={}",
                node.depth,
                node.branch,
                node.base,
                node.worktree
                    .as_ref()
                    .map(|worktree| worktree.path.display().to_string())
                    .unwrap_or_default(),
                node.review_url.clone().unwrap_or_default(),
                node.problems
                    .iter()
                    .map(StackProblem::label)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        return lines;
    }

    lines.push(format!("repo={}", graph.repo.display()));
    for node in &graph.nodes {
        let indent = "  ".repeat(node.depth);
        let path = node
            .worktree
            .as_ref()
            .map(|worktree| worktree.path.display().to_string())
            .unwrap_or_else(|| "-".to_string());
        let review = node.review_url.as_deref().unwrap_or("-");
        let problems = node
            .problems
            .iter()
            .map(StackProblem::label)
            .collect::<Vec<_>>()
            .join(",");
        let problems = if problems.is_empty() {
            String::new()
        } else {
            format!(" problems={problems}")
        };
        lines.push(format!(
            "{indent}{}\tbase={}\tpath={}\treview={}{}",
            node.branch, node.base, path, review, problems
        ));
    }
    lines
}

fn stack_by_branch<'a>(
    state: &'a State,
    repo: &Path,
) -> BTreeMap<String, &'a crate::state::StackBranch> {
    state
        .stack_branches
        .iter()
        .filter(|item| path_eq(&item.repo, repo))
        .map(|item| (item.branch.clone(), item))
        .collect()
}

fn visit_graph_node(
    state: &State,
    repo: &Path,
    branch: &str,
    by_branch: &BTreeMap<String, &crate::state::StackBranch>,
    visiting: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    nodes: &mut Vec<StackNode>,
) -> Result<usize> {
    if visited.contains(branch) {
        return Ok(nodes
            .iter()
            .find(|node| node.branch == branch)
            .map(|node| node.depth)
            .unwrap_or(0));
    }
    if !visiting.insert(branch.to_string()) {
        return Err(anyhow!("stack metadata contains a cycle at {branch}"));
    }
    let item = by_branch
        .get(branch)
        .ok_or_else(|| anyhow!("no stack metadata for {branch}"))?;
    let mut problems = Vec::new();
    let depth = if by_branch.contains_key(&item.base) {
        visit_graph_node(state, repo, &item.base, by_branch, visiting, visited, nodes)? + 1
    } else {
        0
    };
    if !by_branch.contains_key(&item.base) && item.base != "main" && item.base != "master" {
        // The base may be a default branch or an untracked local branch. Mark only as metadata-missing.
        problems.push(StackProblem::MissingBaseMetadata(item.base.clone()));
    }
    let worktree = state.worktree_for_branch(repo, branch).cloned();
    if worktree.is_none() {
        problems.push(StackProblem::MissingWorktree);
    }
    visiting.remove(branch);
    visited.insert(branch.to_string());
    nodes.push(StackNode {
        branch: branch.to_string(),
        base: item.base.clone(),
        depth,
        worktree,
        review_url: item.review_url.clone(),
        problems,
    });
    Ok(depth)
}

fn resolve_leaf(state: &State, repo: &Path, leaf: Option<&str>) -> Result<String> {
    if let Some(leaf) = leaf {
        if state.stack_branch(repo, leaf).is_none() {
            return Err(anyhow!("no stack metadata for branch {leaf}"));
        }
        return Ok(leaf.to_string());
    }
    let branches: Vec<_> = state
        .stack_branches
        .iter()
        .filter(|item| path_eq(&item.repo, repo))
        .map(|item| item.branch.clone())
        .collect();
    let bases: BTreeSet<_> = state
        .stack_branches
        .iter()
        .filter(|item| path_eq(&item.repo, repo))
        .map(|item| item.base.clone())
        .collect();
    let leaves: Vec<_> = branches
        .into_iter()
        .filter(|branch| !bases.contains(branch))
        .collect();
    match leaves.as_slice() {
        [leaf] => Ok(leaf.clone()),
        [] => Err(anyhow!("no stack leaf found; pass --branch")),
        _ => Err(anyhow!("multiple stack leaves found; pass --branch")),
    }
}

fn ancestry(state: &State, repo: &Path, leaf: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current = leaf.to_string();
    loop {
        if !seen.insert(current.clone()) {
            return Err(anyhow!("stack metadata contains a cycle at {current}"));
        }
        out.push(current.clone());
        let item = state
            .stack_branch(repo, &current)
            .ok_or_else(|| anyhow!("no stack metadata for {current}"))?;
        if state.stack_branch(repo, &item.base).is_some() {
            current = item.base.clone();
        } else {
            break;
        }
    }
    out.reverse();
    Ok(out)
}

fn path_eq(left: &Path, right: &Path) -> bool {
    let canonical_left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let canonical_right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    canonical_left == canonical_right
}

fn conflict_blocks(path: &Path) -> Result<Vec<ConflictBlock>> {
    let text = fs::read_to_string(path)?;
    let mut blocks = Vec::new();
    let mut start = None;
    let mut separator = None;
    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        if line.starts_with("<<<<<<<") {
            start = Some(line_no);
            separator = None;
        } else if line.starts_with("=======") && start.is_some() {
            separator = Some(line_no);
        } else if line.starts_with(">>>>>>>") {
            if let (Some(start_line), Some(separator_line)) = (start, separator) {
                blocks.push(ConflictBlock {
                    start_line,
                    separator_line,
                    end_line: line_no,
                });
            }
            start = None;
            separator = None;
        }
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{StackBranch, Worktree};
    use time::OffsetDateTime;

    fn stack_item(repo: &Path, branch: &str, base: &str) -> StackBranch {
        let now = OffsetDateTime::now_utc();
        StackBranch {
            repo: repo.to_path_buf(),
            branch: branch.to_string(),
            base: base.to_string(),
            review_url: None,
            review_id: None,
            created: now,
            updated: now,
        }
    }

    #[test]
    fn plans_rebase_root_to_leaf() {
        let repo = PathBuf::from("/repo");
        let mut state = State::default();
        state
            .stack_branches
            .push(stack_item(&repo, "feat/a", "main"));
        state
            .stack_branches
            .push(stack_item(&repo, "feat/b", "feat/a"));
        state.worktrees.push(Worktree {
            path: PathBuf::from("/wt/a"),
            repo: repo.clone(),
            branch: "feat/a".to_string(),
            created: OffsetDateTime::now_utc(),
        });
        state.worktrees.push(Worktree {
            path: PathBuf::from("/wt/b"),
            repo: repo.clone(),
            branch: "feat/b".to_string(),
            created: OffsetDateTime::now_utc(),
        });
        let plan = plan_rebase(&state, &repo, Some("feat/b"), None).unwrap();
        assert_eq!(plan.steps[0].branch, "feat/a");
        assert_eq!(plan.steps[0].base, "main");
        assert_eq!(plan.steps[1].branch, "feat/b");
        assert_eq!(plan.steps[1].base, "feat/a");
    }

    #[test]
    fn preserves_user_body_while_replacing_links() {
        let existing = "Title\n\n<!-- shirei-stack-links:start -->\nold\n<!-- shirei-stack-links:end -->\n\nTail";
        let new_body = replace_stack_links_block(
            existing,
            "<!-- shirei-stack-links:start -->\nnew\n<!-- shirei-stack-links:end -->",
        )
        .unwrap();
        assert_eq!(
            new_body,
            "Title\n\n<!-- shirei-stack-links:start -->\nnew\n<!-- shirei-stack-links:end -->\n\nTail"
        );
    }

    #[test]
    fn appends_links_without_rewriting_user_body() {
        let new_body = replace_stack_links_block("User body\n", "block").unwrap();
        assert_eq!(new_body, "User body\n\n\nblock");
    }
}
