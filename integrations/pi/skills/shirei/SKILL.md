---
name: shirei
description: Use the Shirei terminal session manager and its worktree-aware project switching and review workflows. Load when opening or listing Shirei sessions, creating branch worktrees, creating standalone PRs or MRs, managing stacked PRs, cascade rebasing stack branches, finding unfinished worktrees, interactively deleting worktrees, scanning Shirei state, generating shell completions, configuring tmux or Herdr, or troubleshooting the shirei CLI.
compatibility: Requires shirei and one configured backend (tmux or Herdr); interactive commands also require fzf. Review creation, unfinished-review detection, and stack submit require authenticated gh >= 2.40.0 or glab >= 1.50.0.
---

# Shirei CLI

Shirei is a terminal session manager with worktree-aware directory tracking. It combines live tmux sessions or Herdr workspaces, recent directories, registered Git worktrees, and configured project roots in one picker.

## Retrieve Current Behavior First

Shirei may evolve. Before acting, inspect the installed version and relevant help:

```bash
command -v shirei
shirei --version
shirei --help
shirei <command> --help
```

When working in the Shirei source repository, read `README.md` and the relevant source:

| Concern | Source |
|---|---|
| CLI flows | `src/main.rs` |
| Configuration | `src/config.rs` |
| State | `src/state.rs` |
| Git branches/worktrees | `src/git.rs` |
| Forge review detection | `src/forge.rs` |
| Picker behavior | `src/picker.rs` |
| Backend abstraction | `src/session.rs` |
| tmux | `src/tmux.rs` |
| Herdr | `src/herdr.rs` |
| Automatic project setup | `src/install.rs` |
| Pi extension | `integrations/pi/shirei.ts` |

Prefer installed help and source over remembered flags or stale documentation.

## Command Selection

| Intent | Command | Notes |
|---|---|---|
| Open the main picker | `shirei` or `shirei pick` | Interactive; selecting a directory opens or reuses a backend session. |
| Open a branch worktree | `shirei branch [NAME] [-b BASE]` | Interactive unless enough information is supplied. |
| List recent directories | `shirei recent` | Read-only; prints timestamp and path. |
| List live sessions | `shirei sessions` | Read-only; uses the configured backend. |
| Close sessions | `shirei kill` | Interactive multi-select and destructive. |
| List tracked worktrees | `shirei worktrees` | Reads Shirei's registry, not live Git state. |
| Find unfinished work | `shirei worktrees unfinished` | Read-only; consults Git and available forge CLIs. |
| Delete worktrees | `shirei worktrees prune` | Interactive, confirmed, and destructive. |
| Rebuild the registry | `shirei worktrees scan` | Replaces the registry from discovered Git worktrees. |
| Create standalone review | `shirei pr create [--base BASE] [--draft]` | Pushes the current branch and creates or reuses its PR/MR. |
| Preview standalone review | `shirei pr create --dry-run` | Validates and previews without pushing or contacting the forge. |
| Create stacked PR branch | `shirei stack branch NAME --base BASE` | Creates/opens worktree and records review base. |
| List stacked PRs | `shirei stack list` | Read-only stack metadata report. |
| Cascade rebase stack | `shirei stack rebase --branch LEAF [--dry-run]` | Mutating unless `--dry-run`; stops on conflicts. |
| Submit stacked PRs | `shirei stack submit --branch LEAF [--dry-run]` | Pushes and edits GitHub PRs or GitLab MRs unless `--dry-run`. |
| Generate completions | `shirei completions <shell>` | Supports Bash, Elvish, Fish, PowerShell, and Zsh. |

Shirei has no JSON output, noninteractive picker mode, or command that changes the parent shell's directory.

## Main Picker

```bash
shirei
```

The picker shows:

1. Live backend sessions/workspaces
2. Tracked stack branches with registered worktrees
3. Recent directories
4. Registered linked worktrees
5. Directories found under configured roots

Selecting a live entry activates it. Selecting a directory creates or reuses a session/workspace, activates it, and records the directory as recent. Cancelling fzf normally exits successfully without taking action.

Use the picker only when the user wants an interactive terminal flow. Do not launch it in unattended execution.

## Branch Worktrees

Run `shirei branch` from anywhere inside the target Git repository.

### Existing local or remote branch

```bash
shirei branch feature/name
```

If the branch exists only as `origin/feature/name`, Shirei creates a local branch tracking that remote ref in a linked worktree.

### New branch from a known base

```bash
shirei branch feature/name --base main
```

Supplying both name and base avoids fzf for a new branch. If the branch already exists, `--base` is ignored with a warning.

Without `NAME`, Shirei opens a branch picker. For a new named branch without `--base`, it may open a second picker for the base.

### Side effects

`shirei branch` can:

- Create a Git branch and linked worktree
- Create directories below `worktree_root`
- Update Shirei worktree and recent state
- Create or reuse a tmux session or Herdr workspace
- Switch or attach the user's terminal client
- Run repository-controlled setup commands when `run_install_on_create = true`

Automatic setup detects, in order:

1. `Makefile` with an `install:` target -> `make install`
2. `package.json` -> `bun`, `pnpm`, `yarn`, or `npm install`
3. `Cargo.toml` -> `cargo build`

Before an agent runs `shirei branch`, get explicit user intent and inspect configuration. Prefer `run_install_on_create = false` unless the user wants setup scripts executed.

Shirei computes the target as:

```text
<worktree_root>/<sanitized-repo-name>/<sanitized-branch-name>
```

Sanitization replaces characters other than ASCII letters, digits, `-`, and `_` with `-`. Different names can collide, so inspect the target when a collision is plausible.

## Sessions

List sessions:

```bash
shirei sessions
```

The configured backend is tmux or Herdr. The leading `*` marks the attached tmux session or focused Herdr workspace.

Close sessions:

```bash
shirei kill
```

Use Tab to select entries and Enter to close them. There is no confirmation after Enter. Never invoke `kill` without explicit user intent and an interactive terminal. It terminates live sessions and their processes; it does not remove directories or Shirei state.

## Worktrees

List registry entries:

```bash
shirei worktrees
```

A leading `!` means the recorded path is missing. This reports saved state and does not reconcile against Git.

### Find unfinished work

```bash
shirei worktrees unfinished
```

Potentially unfinished worktrees include dirty, unmerged, merge-unknown, detached, missing, and inspection-error entries. Named branches are checked with `gh` and `glab` when available:

- matching active or merged reviews suppress a candidate
- local commits newer than a review produce `review=stale`
- unavailable or unverifiable review state produces `review=unknown`

The command is conservative: uncertainty remains visible instead of being treated as finished. Run `shirei worktrees scan` first to include worktrees created outside Shirei.

### Delete worktrees

```bash
shirei worktrees prune
```

This is an interactive destructive picker. It shows dirty, merge, detached, missing, current-directory, and primary-worktree state. The flow is:

1. Use Tab to select worktrees in fzf.
2. Press Enter to review selected paths.
3. Type `delete` exactly to confirm.

For every confirmed selection, Shirei attempts to remove:

- the worktree directory
- Git worktree metadata
- the associated local branch, even when dirty or unmerged
- the Shirei worktree entry
- the Shirei recent-directory entry

Remote branches are never deleted. The primary worktree is always refused. If branch deletion fails after directory removal, Shirei retains a retryable state entry.

Never invoke prune unattended or pipe an automatic confirmation into it. Require explicit user intent and an interactive terminal.

### Rebuild the registry

```bash
shirei worktrees scan
```

`scan` discovers repositories from known entries and configured roots, runs `git worktree list --porcelain`, and replaces the saved registry. A scan error for one repository can cause that repository's previous entries to disappear. Back up state first when registry continuity matters.

Configured roots support literal paths and a final `/*` for direct children only. They are not general recursive globs.

## Standalone PRs and MRs

Prefer `shirei pr create` over calling `gh pr create` or `glab mr create` directly. Shirei selects the forge from Git remotes and uses the authenticated host CLI underneath.

The command operates on the current branch and requires a clean worktree. It pushes to `origin`, or the first configured remote when `origin` is absent. `--base` selects the target; otherwise Shirei resolves the selected remote's HEAD branch and fails with guidance when that ref is unavailable. `--title` and either `--body` or `--body-file` override commit-derived text, while `--draft` creates a new review as a draft.

An existing open review targeting the selected base is reused without overwriting its title, body, or draft state. Reviews targeting other branches are left untouched, and closed or merged reviews do not block a new review.

Always run `shirei pr create --dry-run` before mutation. Dry-run validates the Git state and prints the planned branch, commit, remote, push, and review without pushing or invoking `gh` or `glab`. In Pi, use `shirei_pr_create` rather than a shell command; the tool requires confirmation and rejects the operation if the confirmed branch, commit, or remote changes before mutation.

## Stacked PRs

`shirei stack branch NAME --base BASE` creates or reuses a worktree/session and records `NAME -> BASE` as stack metadata. Use it when the user wants stacked PR/MR workflows instead of a standalone branch.

Common commands:

```bash
shirei stack branch feature/api --base main
shirei stack branch feature/ui --base feature/api
shirei stack list
shirei stack rebase --branch feature/ui --dry-run
```

`shirei stack rebase --branch LEAF` cascades from the root stack branch through `LEAF`, running Git rebase one worktree at a time. It is mutating and requires clean worktrees. On conflict it stops, records progress, prints conflicted files, and expects the user to resolve/stage files before `shirei stack rebase --continue`; `--abort` aborts the active cascade rebase.

In Pi, use the `shirei_stack_rebase_assist` tool only when the user explicitly asks for LLM-assisted conflict repair. The tool confirms, switches to the fastest available model in the default order `openai/gpt-5.6-luna`, `openai/gpt-5.6-terra`, `openai/gpt-5.6-sol`, sends focused conflict prompts, verifies markers/unmerged paths are gone, stages resolved files, continues the rebase, and restores the previous model.

`shirei stack submit --branch LEAF` pushes stack branches and uses `gh` for GitHub PRs or `glab` for GitLab MRs. It updates descriptions only by appending or replacing the managed block between `<!-- shirei-stack-links:start -->` and `<!-- shirei-stack-links:end -->`; user-authored description text outside that block must be preserved exactly. Use `--dry-run` before non-dry-run submit.

## Shell Completions

Generate static command, subcommand, and option completions:

```bash
shirei completions zsh
shirei completions bash
shirei completions fish
```

Branch names are not dynamically completed; use the interactive branch picker when needed.

## Pi Extension

The bundled extension is `integrations/pi/shirei.ts`. It registers `/shirei` commands without adding output to LLM context because reports use `pi.appendEntry()`.

Supported examples:

```text
/shirei branch feature/name
/shirei branch feature/name --base main
/shirei recent
/shirei sessions
/shirei worktrees
/shirei worktrees unfinished
/shirei worktrees scan
/shirei stack list
/shirei stack rebase --branch feature/ui --dry-run
/shirei stack submit --branch feature/ui --dry-run
```

`/shirei branch` and `/shirei stack branch` require Pi to run inside tmux or Herdr. `/shirei worktrees prune` is intentionally blocked because prune requires an interactive terminal. Non-dry-run `/shirei stack submit` is blocked in Pi for now; run it in a terminal. For LLM-assisted cascade rebases, use the `shirei_stack_rebase_assist` tool rather than a plain `/shirei stack rebase` command.

The extension also registers `shirei_pr_create`. Use it for standalone PR/MR creation instead of invoking forge CLIs directly. It accepts an optional repository, base, title, body, and draft flag; it runs `shirei pr create --dry-run`, shows the full preview, and requires confirmation before pushing or creating a review.

The extension also registers the `shirei_stack_rebase_assist` tool. Use it only when the user explicitly asks the current agent to cascade rebase a stack with LLM conflict repair. It requires a leaf branch and optionally accepts a repository, root `--onto` target, model order, and retry limit. It confirms before mutating, chooses the first available/authenticated model from Luna -> Terra -> Sol by default, and restores the previous model on completion/failure.

The extension also registers the `shirei_handoff` tool. Use it only when the user explicitly asks the current agent to delegate work to another Shirei/Pi session. The tool requires:

- an existing target repository path
- a destination branch to create or reuse
- a base commit or branch for new work
- a self-contained first prompt authored by the current agent

The user can edit and confirm the prompt before anything is sent. The extension creates or reuses the destination worktree and Shirei session, delivers to an existing Pi as a follow-up, or starts Pi there when necessary. It restores focus to the source session and does not copy uncommitted changes.

## Configuration

Config lookup order:

1. `$SHIREI_CONFIG`
2. `$XDG_CONFIG_HOME/shirei/config.toml`
3. `~/.config/shirei/config.toml`

Example:

```toml
backend = "tmux"
roots = ["~/projects/*", "~", "~/.config"]
recent_cap = 20
worktree_root = "~/.local/share/shirei/worktrees"
worktree_exclude_prefixes = ["~/Library/Caches"]
run_install_on_create = true
```

State lookup order:

1. `$SHIREI_STATE_DIR/state.json`
2. `$XDG_DATA_HOME/shirei/state.json`
3. `~/.local/share/shirei/state.json`

Only `~` and leading `~/` are expanded in configured paths. Environment variables, recursive globs, and general shell expansion are not supported. Avoid concurrent state-mutating commands because state writes are not locked.

## Terminal Integration

Recommended tmux binding:

```tmux
bind-key -r f run-shell "tmux neww shirei"
```

Recommended Herdr binding:

```toml
[[keys.command]]
key = "prefix+f"
type = "pane"
command = "shirei"
```

Inside tmux, Shirei switches clients; outside tmux, it attaches and may block until detachment. Herdr behaves similarly.

## Agent Safety Rules

1. Retrieve installed help before relying on flags.
2. Treat `shirei`, `pick`, bare `branch`, `kill`, and `worktrees prune` as interactive commands.
3. Never run `worktrees prune` unattended or provide its `delete` confirmation automatically.
4. Treat `branch` and `stack branch` as high-impact because they change Git, state, sessions, and may execute repository code.
5. Use `shirei branch NAME --base REF` or `shirei stack branch NAME --base REF` only after confirming branch, base, and side effects.
6. Prefer `shirei pr create` over direct `gh pr create` or `glab mr create` for standalone reviews.
7. In Pi, use `shirei_pr_create` so standalone review creation is previewed and confirmed.
8. Treat non-dry-run `pr create`, `stack rebase`, and `stack submit` as high-impact; prefer `--dry-run` first.
9. Never let stack submit rewrite a whole PR description. Only the managed previous/next block may be appended or replaced.
10. Require explicit intent before `kill`; it terminates live processes.
11. Back up state before `worktrees scan` when registry continuity matters.
12. Do not infer success from fzf cancellation; cancellation normally exits successfully.
13. Do not assume a saved path or branch is still valid.
14. Do not invent commands or flags; verify them against current help.
