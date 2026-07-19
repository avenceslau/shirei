---
name: shirei
description: Use the Shirei terminal session manager and its worktree-aware project switching workflows. Load when opening or listing Shirei sessions, creating branch worktrees, finding unfinished worktrees, interactively deleting worktrees, scanning Shirei state, generating shell completions, configuring tmux or Herdr, or troubleshooting the shirei CLI.
compatibility: Requires shirei and one configured backend (tmux or Herdr); interactive commands also require fzf. Unfinished-review detection optionally uses gh and glab.
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
| Generate completions | `shirei completions <shell>` | Supports Bash, Elvish, Fish, PowerShell, and Zsh. |

Shirei has no JSON output, noninteractive picker mode, or command that changes the parent shell's directory.

## Main Picker

```bash
shirei
```

The picker shows:

1. Live backend sessions/workspaces
2. Recent directories
3. Registered linked worktrees
4. Directories found under configured roots

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
```

`/shirei branch` requires Pi to run inside tmux or Herdr. `/shirei worktrees prune` is intentionally blocked because prune requires an interactive terminal.

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
4. Treat `branch` as high-impact because it changes Git, state, sessions, and may execute repository code.
5. Use `shirei branch NAME --base REF` only after confirming branch, base, and side effects.
6. Require explicit intent before `kill`; it terminates live processes.
7. Back up state before `worktrees scan` when registry continuity matters.
8. Do not infer success from fzf cancellation; cancellation normally exits successfully.
9. Do not assume a saved path or branch is still valid.
10. Do not invent commands or flags; verify them against current help.
