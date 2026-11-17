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
    pub root_onto_oid: String,
    pub steps: Vec<RebaseStep>,
}

#[derive(Debug, Clone)]
pub struct RebaseStep {
    pub branch: String,
    pub base: String,
    pub worktree: PathBuf,
    pub before_head_oid: String,
    pub old_base_oid: String,
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
    pub branch: String,
    /// One-based position of `branch` in the merge order.
    pub position: usize,
    pub previous: Option<ReviewLink>,
    pub next: Vec<ReviewLink>,
    pub merge_order: Vec<ReviewLink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewLink {
    pub branch: String,
    /// `None` while the branch has no open review.
    pub url: Option<String>,
}

impl ReviewLink {
    fn markdown(&self) -> String {
        match &self.url {
            Some(url) => format!("[{}]({url})", self.branch),
            None => format!("{} (no review yet)", self.branch),
        }
    }
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
    let git_worktrees = crate::git::list_worktrees(repo)?;
    let canonical_repo = std::fs::canonicalize(repo)
        .map_err(|error| anyhow!("canonicalize repository {}: {error}", repo.display()))?;
    let common_dir = crate::git::common_dir(&canonical_repo)?;
    let mut steps = Vec::new();
    for branch in branches {
        let item = state
            .stack_branch(repo, &branch)
            .ok_or_else(|| anyhow!("no stack metadata for {branch}"))?;
        let worktree = state
            .worktree_for_branch(repo, &branch)
            .ok_or_else(|| anyhow!("no tracked worktree for stack branch {branch}"))?;
        let canonical_worktree = std::fs::canonicalize(&worktree.path).map_err(|error| {
            anyhow!(
                "canonicalize stack worktree {}: {error}",
                worktree.path.display()
            )
        })?;
        let worktree_common_dir = crate::git::common_dir(&canonical_worktree)?;
        if worktree_common_dir != common_dir {
            return Err(anyhow!(
                "stack worktree {} belongs to a different repository",
                canonical_worktree.display()
            ));
        }
        let registered = git_worktrees.iter().any(|candidate| {
            std::fs::canonicalize(&candidate.path)
                .ok()
                .is_some_and(|path| path == canonical_worktree)
                && candidate.branch.as_deref() == Some(branch.as_str())
        });
        if !registered {
            return Err(anyhow!(
                "{} is not the Git worktree for stack branch {branch}",
                canonical_worktree.display()
            ));
        }
        let current_branch = crate::git::current_branch(&canonical_worktree)?;
        if current_branch != branch {
            return Err(anyhow!(
                "stack worktree {} has branch {current_branch}, expected {branch}",
                canonical_worktree.display()
            ));
        }
        let before_head_oid = crate::git::head_oid(&canonical_worktree)?;
        let old_base_oid = base_oid(
            &canonical_worktree,
            &branch,
            &item.base,
            item.base_oid.as_deref(),
        )?;
        steps.push(RebaseStep {
            branch,
            base: item.base.clone(),
            worktree: canonical_worktree,
            before_head_oid,
            old_base_oid,
        });
    }
    let root = steps
        .first()
        .ok_or_else(|| anyhow!("stack rebase plan is empty"))?;
    let root_target = onto.unwrap_or(&root.base);
    let root_onto_oid = crate::git::rev_parse_commit(repo, root_target)?;
    Ok(RebasePlan {
        repo: canonical_repo,
        leaf,
        root_onto_oid,
        steps,
    })
}

/// The commit of `base` that `branch` is built on. When `base` was rewritten
/// after `recorded` was stored, merge-base finds an older commit and a rebase
/// from it would replay the base's old commits, so the recorded commit wins
/// while it is still in `branch`'s history and newer than the merge-base.
pub fn base_oid(repo: &Path, branch: &str, base: &str, recorded: Option<&str>) -> Result<String> {
    let merge_base = crate::git::merge_base(repo, branch, base)?;
    let Some(recorded) = recorded else {
        return Ok(merge_base);
    };
    if crate::git::rev_parse_commit(repo, recorded).is_err() {
        return Ok(merge_base);
    }
    if crate::git::is_ancestor(repo, recorded, branch)?
        && crate::git::is_ancestor(repo, &merge_base, recorded)?
    {
        return Ok(recorded.to_string());
    }
    Ok(merge_base)
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

/// Links for every review in `reviews`, which must be in merge order (root
/// first), as `review_chain` and `plan_submit` return them.
pub fn build_stack_links(reviews: &[SubmittedReview]) -> BTreeMap<String, StackLinks> {
    let link = |review: &SubmittedReview| ReviewLink {
        branch: review.branch.clone(),
        url: review.url.clone(),
    };
    let merge_order: Vec<ReviewLink> = reviews.iter().map(link).collect();
    let mut links = BTreeMap::new();
    for (index, review) in reviews.iter().enumerate() {
        let previous = reviews
            .iter()
            .find(|parent| parent.branch == review.base)
            .map(link);
        let next = reviews
            .iter()
            .filter(|child| child.base == review.branch)
            .map(link)
            .collect();
        links.insert(
            review.branch.clone(),
            StackLinks {
                branch: review.branch.clone(),
                position: index + 1,
                previous,
                next,
                merge_order: merge_order.clone(),
            },
        );
    }
    links
}

pub fn render_stack_links(links: &StackLinks) -> String {
    let mut out = Vec::new();
    out.push("<!-- shirei-stack-links:start -->".to_string());
    out.push(format!(
        "Stack: {} of {}",
        links.position,
        links.merge_order.len()
    ));
    match &links.previous {
        Some(previous) => out.push(format!("- Previous: {}", previous.markdown())),
        None => out.push("- Previous: none".to_string()),
    }
    match links.next.as_slice() {
        [] => out.push("- Next: none".to_string()),
        [single] => out.push(format!("- Next: {}", single.markdown())),
        many => {
            out.push("- Next:".to_string());
            for next in many {
                out.push(format!("  - {}", next.markdown()));
            }
        }
    }
    out.push(String::new());
    out.push("Merge order:".to_string());
    for (index, entry) in links.merge_order.iter().enumerate() {
        if entry.branch == links.branch {
            out.push(format!("{}. **{}** (this review)", index + 1, entry.branch));
        } else {
            out.push(format!("{}. {}", index + 1, entry.markdown()));
        }
    }
    out.push("<!-- shirei-stack-links:end -->".to_string());
    out.join("\n")
}

/// The stack `branch` belongs to, in merge order: its tracked ancestors, the
/// branch, then its descendants while each has exactly one tracked child.
pub fn review_chain(state: &State, repo: &Path, branch: &str) -> Result<Vec<String>> {
    let mut chain = ancestry(state, repo, branch)?;
    let mut current = branch.to_string();
    loop {
        let children: Vec<&str> = state
            .stack_branches
            .iter()
            .filter(|item| path_eq(&item.repo, repo) && item.base == current)
            .map(|item| item.branch.as_str())
            .collect();
        let [child] = children.as_slice() else {
            return Ok(chain);
        };
        if chain.iter().any(|seen| seen == child) {
            return Err(anyhow!("stack metadata contains a cycle at {child}"));
        }
        chain.push(child.to_string());
        current = child.to_string();
    }
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
    fn renders_position_neighbours_and_merge_order() {
        let review = |branch: &str, base: &str, url: Option<&str>| SubmittedReview {
            branch: branch.into(),
            base: base.into(),
            id: None,
            url: url.map(str::to_string),
        };
        let links = build_stack_links(&[
            review("feat/a", "main", Some("https://forge/1")),
            review("feat/b", "feat/a", Some("https://forge/2")),
            review("feat/c", "feat/b", None),
        ]);

        assert_eq!(
            render_stack_links(&links["feat/b"]),
            "<!-- shirei-stack-links:start -->\n\
             Stack: 2 of 3\n\
             - Previous: [feat/a](https://forge/1)\n\
             - Next: feat/c (no review yet)\n\
             \n\
             Merge order:\n\
             1. [feat/a](https://forge/1)\n\
             2. **feat/b** (this review)\n\
             3. feat/c (no review yet)\n\
             <!-- shirei-stack-links:end -->"
        );
        assert!(render_stack_links(&links["feat/a"]).contains("Stack: 1 of 3\n- Previous: none"));
    }

    #[test]
    fn appends_links_without_rewriting_user_body() {
        let new_body = replace_stack_links_block("User body\n", "block").unwrap();
        assert_eq!(new_body, "User body\n\n\nblock");
    }
}
