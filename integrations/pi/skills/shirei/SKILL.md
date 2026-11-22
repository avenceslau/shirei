---
name: shirei
description: Use the Shirei terminal session manager and its worktree-aware project switching and review workflows. Load when opening or listing Shirei sessions, creating branch worktrees, creating standalone PRs or MRs, managing stacked PRs, cascade rebasing stack branches, finding unfinished worktrees, interactively deleting worktrees, scanning Shirei state, generating shell completions, configuring tmux, Herdr, or Rex, or troubleshooting the shirei CLI.
compatibility: Requires shirei and one configured backend (tmux, Herdr, or Rex); interactive commands also require fzf. Review creation, unfinished-review detection, and stack submit require authenticated gh >= 2.40.0 or glab >= 1.50.0.
---

# Shirei CLI

Shirei is a terminal session manager with worktree-aware directory tracking. It combines live tmux sessions, Herdr workspaces, or Rex sessions with recent directories, registered Git worktrees, and configured project roots in one picker.

## Retrieve Current Behavior First

Shirei may evolve. When a flag below is rejected, or the version matters, ask the installed binary once instead of reading `--help` pages:

```bash
shirei capabilities --json   # version, git SHA, every subcommand and flag
```

Older binaries without `capabilities` predate everything in this skill marked as needing it; fall back to `shirei --version` and `shirei <command> --help`.

When working in the Shirei source repository, read `README.md` and the relevant source:

| Concern | Source |
|---|---|
| CLI flows | `src/main.rs` |
| Configuration | `src/config.rs` |
| State | `src/state.rs` |
| Git branches/worktrees | `src/git.rs` |
| Forge review detection | `src/forge.rs` |
| Review title/body rules | `src/review.rs` |
| Stack graph and links | `src/stack.rs` |
| Picker behavior | `src/picker.rs` |
| Backend abstraction | `src/session.rs` |
| tmux | `src/tmux.rs` |
| Herdr | `src/herdr.rs` |
| Rex | `src/rex.rs` |
| Automatic project setup | `src/install.rs` |
| Pi extension | `integrations/pi/shirei.ts` |

Prefer installed help and source over remembered flags or stale documentation.

## Command Selection

| Intent | Command | Notes |
|---|---|---|
| Open the main picker | `shirei` or `shirei pick` | Interactive; selecting a directory opens or reuses a backend session. |
| Create a worktree as an agent | `shirei branch NAME --base BASE --no-open --print-path [--sparse DIR...] [--install=sync]` | Prints only the path; no session or terminal switch. Use this instead of `git worktree add`. |
| Open a branch worktree | `shirei branch [NAME] [-b BASE]` | Interactive unless enough information is supplied. |
| Register a worktree made with git | `shirei worktrees add PATH` | Upserts one entry; no full scan. |
| List recent directories | `shirei recent` | Read-only; prints timestamp and path. |
| List live sessions | `shirei sessions` | Read-only; uses the configured backend. |
| Close sessions | `shirei kill` | Interactive multi-select and destructive. |
| List tracked worktrees | `shirei worktrees` | Reads Shirei's registry, not live Git state. |
| Find unfinished work | `shirei worktrees unfinished` | Read-only; consults Git and available forge CLIs. |
| Delete worktrees | `shirei worktrees prune` | Interactive, confirmed, and destructive. |
| Preview merged-worktree cleanup | `shirei worktrees prune --merged [PATH...]` | Read-only without `--yes`; prints `would remove` and `skip ... reason=`. |
| Delete merged worktrees | `shirei worktrees prune --merged --yes [PATH...]` | Destructive; only clean, merged, non-primary worktrees. |
| Drop entries for deleted paths | `shirei worktrees prune --missing` | Registry only; deletes nothing on disk. |
| Rebuild the registry | `shirei worktrees scan` | Rebuilds the registry; keeps entries of repos that fail to scan. |
| Create standalone review | `shirei pr create [--base BASE] [--draft]` | Pushes the current branch and creates or reuses its PR/MR. |
| Preview standalone review | `shirei pr create --dry-run` | Validates and previews without pushing or contacting the forge. |
| Create stacked PR branch | `shirei stack branch NAME --base BASE [--no-open --print-path]` | Creates the worktree and records review base and base commit. |
| List stacked PRs | `shirei stack list` | Read-only stack metadata report. |
| Cascade rebase stack | `shirei stack rebase --branch LEAF [--dry-run]` | Mutating unless `--dry-run`; stops on conflicts. |
| Submit stacked PRs | `shirei stack submit --branch LEAF [--dry-run] [--remote R]` | Force-pushes with a lease and edits GitHub PRs or GitLab MRs unless `--dry-run`. |
| List commands and flags | `shirei capabilities --json` | Read-only; version, git SHA, subcommands, flags. |
| Generate completions | `shirei completions <shell>` | Supports Bash, Elvish, Fish, PowerShell, and Zsh. |

Shirei has no noninteractive picker mode or command that changes the parent shell's directory. Commands work the same from the main checkout or any linked worktree: repositories are keyed by their main worktree.

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

If the branch exists only as `origin/feature/name`, Shirei creates a local branch tracking that remote ref in a linked worktree. If the branch is already checked out in another worktree, Shirei reuses that worktree and registers its real path.

### New branch from a known base

```bash
shirei branch feature/name --base main
```

Supplying both name and base avoids fzf for a new branch. If the branch already exists, `--base` is ignored with a warning. A new branch never tracks its base (`--no-track`), so the first `git push -u` sets its upstream.

### As an agent

Always create worktrees with Shirei, never with raw `git worktree add`:

```bash
W=$(shirei branch feature/name --base origin/main --no-open --print-path)
W=$(shirei branch feature/name --base origin/main --no-open --print-path --install=sync --sparse packages/api)
```

- `--no-open` creates and registers the worktree without creating a session or switching the user's terminal. It is the default when stdin is not a terminal or `PI_CODING_AGENT` is set; pass it anyway to be explicit.
- `--print-path` prints only the worktree path on stdout; git progress and notes go to stderr.
- `--install=sync` runs the detected setup command (`pnpm install`, `make install`, `cargo build`, ...) in the foreground and exits non-zero when it fails; the worktree remains. `--install=none` skips it. Without a session, background setup is skipped.
- `--sparse DIR...` checks out only those directories (cone mode). Put it last; it takes every following value.
- `checkout_exclude` in the config keeps files such as `.dev.vars*` from being written, which avoids sandbox `Operation not permitted` failures. A failed checkout removes the partial directory and the new branch, so rerunning is safe.

If a worktree was already created with git, register it with `shirei worktrees add PATH` instead of running `shirei worktrees scan`.

Without `NAME`, Shirei opens a branch picker. For a new named branch without `--base`, it may open a second picker for the base.

### Side effects

`shirei branch` can:

- Create a Git branch and linked worktree
- Create directories below `worktree_root`
- Update Shirei worktree and recent state
- Create or reuse a tmux session, Herdr workspace, or Rex session (not with `--no-open`)
- Switch or attach the user's terminal client (not with `--no-open`)
- Run repository-controlled setup commands when `run_install_on_create = true` or `--install=sync`

Automatic setup detects, in order:

1. `Makefile` with an `install:` target -> `make install`
2. `package.json` -> `bun`, `pnpm`, `yarn`, or `npm install`
3. `Cargo.toml` -> `cargo build`

Pass `--install=none` unless the user wants setup scripts executed. Run `shirei branch` without `--no-open` only when the user wants their terminal switched.

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

The configured backend is tmux, Herdr, or Rex. The leading `*` marks the current backend session or workspace.

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

Never invoke the interactive prune unattended or pipe an automatic confirmation into it. Require explicit user intent and an interactive terminal.

When the user asks to clean up worktrees, use the non-interactive form instead: run `shirei worktrees prune --merged [PATH...]` first, show the `would remove` and `skip` lines, then rerun with `--yes` after the user agrees. It only deletes clean worktrees whose HEAD is in a remote default branch or a merged review, and never the primary or current worktree. `shirei worktrees prune --missing` drops registry entries for deleted directories without touching the disk.

### Rebuild the registry

```bash
shirei worktrees scan
```

`scan` discovers repositories from known entries and configured roots, resolves each to its main worktree, runs `git worktree list --porcelain`, and replaces the saved registry. A repository that fails to scan keeps its previous entries. To register one worktree, use `shirei worktrees add PATH` instead.

Configured roots support literal paths and a final `/*` for direct children only. They are not general recursive globs.

## Standalone PRs and MRs

Prefer `shirei pr create` over calling `gh pr create` or `glab mr create` directly. Shirei selects the forge from Git remotes and uses the authenticated host CLI underneath.

The command operates on the current branch and requires a clean worktree. It pushes to `--remote`, else the branch's push or upstream remote, else `git config shirei.remote`, else `origin`. `--base` selects the target; otherwise a tracked stack branch targets its stack base, and other branches target the remote's live default branch (`git ls-remote --symref`, with a warning when the local `origin/HEAD` is stale). `--title` and either `--body` or `--body-file` override commit-derived text, while `--draft` creates a new review as a draft.

Shirei writes the deterministic parts of a review and rejects bodies that break the rules, so do not do these by hand:

- Stack navigation: on a tracked stack branch Shirei adds the managed links block (position, previous, next, merge order) and refreshes it on every open review in the stack. A body with its own `Stack` or `Stack position` heading is rejected.
- Ticket prefix: when recent titles use `KEY-NNNN:` (for example `ABC-1725:`), Shirei prepends the ticket from the branch name or commits and replaces `ABC-000`.
- Public scrub: on hosts outside `review.private_hosts`, titles and bodies containing a `review.public_denylist` term are rejected with the hits listed.
- Word budget: bodies over 80/200/350 prose words for small/medium/large changes are rejected with the count and budget.

An existing open review targeting the selected base is reused without overwriting its title, body, or draft state. Reviews targeting other branches are left untouched, and closed or merged reviews do not block a new review.

Always run `shirei pr create --dry-run` before mutation. Dry-run validates the Git state and the rules above and prints the planned branch, commit, remote, push, final title and body, word count, and stack position without pushing or invoking `gh` or `glab`. In Pi, use `shirei_pr_create` rather than a shell command. Before calling it, inspect the branch diff and commits against the review base, then write a short title and description following the `pr-description` skill: why, what changed in behaviour, and how it was tested. The tool requires both fields, previews them, offers Create, Create as draft, or Cancel with a note, and rejects the operation if the confirmed branch, commit, or remote changes before mutation. A returned note is the user's feedback: revise and call the tool again. In a subagent the tool returns the validated review instead of creating it; hand that back to the parent and never fall back to `gh`, `glab`, or `git push`.

## Stacked PRs

`shirei stack branch NAME --base BASE` creates or reuses a worktree (and a session unless `--no-open`) and records `NAME -> BASE` and the base commit as stack metadata. It accepts the same agent flags as `shirei branch`. Use it when the user wants stacked PR/MR workflows instead of a standalone branch.

Common commands:

```bash
shirei stack branch feature/api --base main
shirei stack branch feature/ui --base feature/api
shirei stack list
shirei stack rebase --branch feature/ui --dry-run
```

`shirei stack rebase --branch LEAF` validates every registered branch worktree and snapshots branch-head and old-base OIDs before mutation. The old base is the parent commit Shirei recorded when the parent was rewritten since (so a parent amended or rebased outside Shirei is handled), otherwise the merge-base; do not run `git rebase --onto` by hand. It cascades from the root through `LEAF` with `git rebase --onto <new-parent-oid> <old-parent-oid>` inside each branch's own worktree, so rewritten parent commits are not replayed into children. It requires clean worktrees. On conflict it stops, records OID-aware progress, prints conflicted files, and expects the user to resolve/stage files before `shirei stack rebase --continue`; `--abort` aborts the active cascade rebase.

In Pi, use the `shirei_stack_rebase_assist` tool only when the user explicitly asks for LLM-assisted conflict repair. The tool confirms, switches to the first available model in `SHIREI_REBASE_MODEL_ORDER` (or keeps the current model when it is unset), sends focused conflict prompts, verifies markers/unmerged paths are gone, stages resolved files, continues the rebase, and restores the previous model.

`shirei stack submit --branch LEAF` force-pushes stack branches with `--force-with-lease` on the remote commit seen before the first push, so rebased branches push without a manual force push, and uses `gh` for GitHub PRs or `glab` for GitLab MRs. It updates descriptions only by appending or replacing the managed block between `<!-- shirei-stack-links:start -->` and `<!-- shirei-stack-links:end -->`; user-authored description text outside that block must be preserved exactly. Use `--dry-run` before non-dry-run submit.

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

`/shirei branch` and `/shirei stack branch` require Pi to run inside tmux, Herdr, or Rex and open the session (`--open`). `/shirei worktrees prune` is intentionally blocked because prune requires an interactive terminal. Only the `/shirei stack submit` slash command requires `--dry-run`; Pi does not block running `shirei stack submit` through bash. For LLM-assisted cascade rebases, use the `shirei_stack_rebase_assist` tool rather than a plain `/shirei stack rebase` command.

The extension also registers `shirei_pr_create`. Use it for standalone PR/MR creation instead of invoking forge CLIs directly. It requires a short title and body that follow the `pr-description` skill, accepts an optional repository, base, and draft flag, runs `shirei pr create --dry-run`, shows the final review in the preview, and offers Create, Create as draft, or Cancel with a note before pushing or creating it. Calls run one at a time.

The extension also registers the `shirei_stack_rebase_assist` tool. Use it only when the user explicitly asks the current agent to cascade rebase a stack with LLM conflict repair. It requires a leaf branch and optionally accepts a repository, root `--onto` target, model order, and retry limit. It confirms before mutating, chooses the first available/authenticated model from `modelOrder` or `SHIREI_REBASE_MODEL_ORDER`, else the current model, and restores the previous model on completion/failure.

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
checkout_exclude = [".dev.vars*", ".env*"]

[review]
private_hosts = ["gitlab.example.com"]
public_denylist = ["acme-internal"]
small_max_words = 80
medium_max_words = 200
large_max_words = 350
```

Per-repository Git config: `shirei.remote` (review remote when the branch has no upstream) and `shirei.ticketPrefix` (`ABC`, or `none`).

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

For Rex, set `backend = "rex"` and run `shirei` in a Rex terminal. Set `rex_picker_split = true` to make picker directory selections open a shell in a split in the current session, without Remote Control. Otherwise enable Remote Control under Rex Server in Rex.app's settings to switch sessions. Inside tmux, Shirei switches clients; outside tmux, it attaches and may block until detachment. Herdr behaves similarly, while Rex selects the session in Rex.app.

## Agent Safety Rules

1. When a flag is rejected, check `shirei capabilities --json` once instead of reading `--help` pages.
2. Treat `shirei`, `pick`, bare `branch`, `kill`, and the bare `worktrees prune` as interactive commands.
3. Never run the interactive `worktrees prune` unattended or provide its `delete` confirmation automatically; use `worktrees prune --merged` and add `--yes` only after the user agrees.
4. Create worktrees with `shirei branch NAME --base BASE --no-open --print-path [--sparse DIR...] [--install=sync]` (or `shirei stack branch` for stacks), never with `git worktree add`. Register any worktree made with git through `shirei worktrees add PATH`.
5. Run `branch` or `stack branch` without `--no-open` only when the user wants their terminal switched; pass `--install=none` unless the user wants setup scripts run.
6. Prefer `shirei pr create` over direct `gh pr create` or `glab mr create` for standalone reviews.
7. In Pi, inspect the branch and give `shirei_pr_create` a short title and body following the `pr-description` skill. Leave stack navigation, ticket prefixes, and internal-name checks to Shirei.
8. Treat non-dry-run `pr create`, `stack rebase`, and `stack submit` as high-impact; prefer `--dry-run` first. Pi does not block `shirei stack submit` through bash.
9. Never let stack submit rewrite a whole PR description. Only the managed stack links block may be appended or replaced.
10. Require explicit intent before `kill`; it terminates live processes.
11. Do not force-push or `git rebase --onto` stack branches by hand; `stack rebase` and `stack submit` handle both.
12. Do not infer success from fzf cancellation; cancellation normally exits successfully.
13. Do not assume a saved path or branch is still valid.
14. Do not invent commands or flags; verify them with `shirei capabilities --json`.
