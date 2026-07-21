# shirei

`shirei` is a small terminal session switcher for people who jump between many
projects and git worktrees. It supports tmux sessions and Herdr workspaces.

It gives you one `fzf` picker for:

- live sessions from the configured backend
- recently opened directories
- git worktrees created or discovered by shirei
- project roots under your configured search paths

Selecting a row switches to the existing session when one exists, or starts a
new session in that directory.

## Requirements

`shirei` shells out to existing tools. Install these first:

- `tmux` or `herdr`
- `git`
- `fzf`

Review creation, stack submission, and unfinished-worktree detection use the authenticated CLI for your repository host:

- `gh` 2.40.0 or newer for GitHub pull requests
- `glab` 1.50.0 or newer for GitLab merge requests

Shirei checks the selected CLI version before pushing a branch for review and returns an update instruction when it is missing, unparseable, or too old.

## Install

From crates.io:

```bash
cargo install shirei
```

From source:

```bash
git clone https://github.com/avenceslau/shirei
cd shirei
cargo install --path .
```

Both install a single `shirei` binary into `~/.cargo/bin`.

## Shell Completions

Generate completions for Bash, Elvish, Fish, PowerShell, or Zsh:

```bash
shirei completions zsh
```

For Zsh, install the generated function somewhere in `fpath` before `compinit`:

```zsh
mkdir -p ~/.zfunc
shirei completions zsh > ~/.zfunc/_shirei
fpath=(~/.zfunc $fpath)
```

For Fish:

```fish
mkdir -p ~/.config/fish/completions
shirei completions fish > ~/.config/fish/completions/shirei.fish
```

For Bash, source the generated script from your shell configuration:

```bash
source <(shirei completions bash)
```

## Pi Integration

The repository includes a Pi extension and Shirei skill:

```bash
mkdir -p ~/.pi/agent/extensions ~/.agents/skills/shirei
cp integrations/pi/shirei.ts ~/.pi/agent/extensions/shirei.ts
cp integrations/pi/skills/shirei/SKILL.md ~/.agents/skills/shirei/SKILL.md
```

Run `/reload` in Pi, then use commands such as:

```text
/shirei branch feature/name
/shirei branch feature/name --base main
/shirei sessions
/shirei worktrees unfinished
/shirei stack list
/shirei stack rebase --branch feature/name --dry-run
```

The extension renders command reports with `pi.appendEntry()`, so they do not
enter LLM context. Interactive prune remains terminal-only. Non-dry-run stack
submit is terminal-only for now. For a standalone review, ask Pi to use
`shirei_pr_create`; the agent inspects the branch, authors the title and
review description, previews the complete push and review, then requires
confirmation before running `shirei pr create`. For LLM-assisted cascade rebases, ask Pi to
use `shirei_stack_rebase_assist`; it confirms first and then uses the fastest
available GPT-5.6 Luna/Terra/Sol model to fix conflicts.

The extension also registers a `shirei_handoff` tool for delegating work to
another Pi session. Ask the current agent to hand a task to a repository,
branch, and base ref. The agent authors a self-contained first prompt, the
extension lets you edit it, then Shirei creates or reuses the worktree session.
An existing Pi in the destination receives the prompt as a follow-up; otherwise
the extension starts Pi there. The source session remains open and regains
focus, and uncommitted changes are not copied.

## tmux Binding

Add a binding to your tmux config:

```tmux
bind-key -r f run-shell "tmux neww shirei"
bind-key -r n display-popup -E -w 80% -h 80% "shirei branch"
```

Press `prefix + f` to open the picker in a new tmux window. Press `prefix + n`
to create or select a branch from a temporary popup; it closes after shirei
switches to the worktree session. To create a branch, type its new name and
press Enter, then select its base branch.

## Herdr Binding

With Herdr, set `backend = "herdr"` and run `shirei` from a Herdr pane or a
normal shell. Shirei maps projects to Herdr workspaces. From a normal shell it
starts the Herdr server when needed and attaches after selecting a workspace.

Add a command binding to `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+f"
type = "pane"
command = "shirei"

[[keys.command]]
key = "prefix+n"
type = "pane"
command = "shirei branch"
```

Press your Herdr prefix followed by `f` to open the picker, or `n` to create or
select a branch in a temporary pane. Type a new name and press Enter to create a
branch, then select its base branch. To use `Ctrl-a` as the prefix, add this above
the command bindings:

```toml
[keys]
prefix = "ctrl+a"
```

## Common Workflows

Open the picker:

```bash
shirei
```

Create or attach to a worktree for a branch:

```bash
shirei branch feature/my-change
```

Pick a branch interactively:

```bash
shirei branch
```

Create a new branch from a specific base:

```bash
shirei branch feature/my-change --base main
```

Preview and create a standalone pull request or merge request:

```bash
shirei pr create --dry-run
shirei pr create --title "Describe the change" --body-file /tmp/review.md
```

Create stacked PR worktrees and record their review bases:

```bash
shirei stack branch feature/api --base main
shirei stack branch feature/ui --base feature/api
shirei stack list
```

Preview or run a cascade rebase from the root stack branch through a leaf:

```bash
shirei stack rebase --branch feature/ui --dry-run
shirei stack rebase --branch feature/ui
```

If a rebase stops with conflicts, resolve and stage them, then continue:

```bash
shirei stack rebase --continue
```

In Pi, use the `shirei_stack_rebase_assist` tool when you explicitly want Pi to drive the cascade rebase and ask the fastest available GPT-5.6 Luna/Terra/Sol model to fix conflicts.

Refresh shirei's worktree registry after creating worktrees outside shirei:

```bash
shirei worktrees scan
```

Interactively select tracked worktrees to delete:

```bash
shirei worktrees prune
```

The picker shows dirty, merge, detached, missing, and current-directory state.
After selection, shirei lists the paths and requires typing `delete`. Confirmed
worktrees are permanently removed with their directories, Git worktree
metadata, local branches, and Shirei recent/worktree entries. Remote branches
are never deleted. Dirty and unmerged worktrees can be removed; the primary
worktree is always refused.

Find potentially unfinished worktrees that have no active or merged pull or merge request:

```bash
shirei worktrees unfinished
```

## Commands

| Command | Description |
| --- | --- |
| `shirei` | Open the picker. Same as `shirei pick`. |
| `shirei pick` | Open the picker explicitly. |
| `shirei branch [name]` | Create or attach to a git worktree, register it, and open a session there. |
| `shirei branch [name] --base <ref>` | Use `<ref>` as the start point for a new branch. |
| `shirei recent` | Print recent directories. |
| `shirei sessions` | Print live sessions from the configured backend. |
| `shirei kill` | Select backend sessions or workspaces to close. |
| `shirei completions <shell>` | Generate a shell completion script. |
| `shirei worktrees` | List tracked worktrees. |
| `shirei worktrees unfinished` | List dirty, unmerged, missing, or unclassified worktrees without an active or merged PR or MR. |
| `shirei worktrees scan` | Rebuild tracked worktrees from `git worktree list`. |
| `shirei worktrees prune` | Select and permanently delete worktree directories, metadata, local branches, and Shirei entries. |
| `shirei pr create` | Push the current branch and create or reuse its GitHub PR or GitLab MR. |
| `shirei pr create --dry-run` | Preview standalone review creation without pushing or contacting the forge. |
| `shirei stack branch <name> --base <branch>` | Create or attach to a worktree and record the PR/MR base branch. |
| `shirei stack track <branch> --base <branch>` | Record stack metadata for an existing branch. |
| `shirei stack untrack <branch>` | Remove stack metadata without deleting Git branches or worktrees. |
| `shirei stack list` | Print tracked stack branches and their bases, paths, reviews, and problems. |
| `shirei stack rebase --branch <leaf>` | Cascade rebase a stack from root to leaf, stopping safely on conflicts. |
| `shirei stack rebase --continue` | Continue a stopped cascade rebase after conflicts are resolved and staged. |
| `shirei stack rebase --abort` | Abort the current stopped cascade rebase. |
| `shirei stack submit --branch <leaf>` | Push stack branches, create/update GitHub PRs or GitLab MRs, and update managed previous/next links. |

## Picker Order

Rows are grouped in this order:

1. `SESSION`: live tmux sessions or Herdr workspaces. Tmux sessions are sorted by recent activity; Herdr preserves workspace order.
2. `STACK`: tracked stack branches that have registered worktrees, displayed as `branch base=<base>`.
3. `RECENT`: directories opened recently through shirei.
4. `WORKTREE`: tracked git worktrees.
5. `ROOT`: directories discovered under configured roots.

Earlier groups win. If a live session already represents a directory, that
directory is hidden from later groups so you do not see duplicate entries.

## Configuration

Configuration is optional. By default, shirei reads:

```text
~/.config/shirei/config.toml
```

Example:

```toml
backend = "tmux"
roots = ["~/projects/*", "~", "~/.config"]
recent_cap = 20
worktree_root = "~/.local/share/shirei/worktrees"
worktree_exclude_prefixes = ["~/Library/Caches"]
run_install_on_create = true
```

Settings:

| Setting | Description |
| --- | --- |
| `backend` | Session backend: `"tmux"` (default) or `"herdr"`. |
| `roots` | Directory globs shown in the `ROOT` picker section. |
| `recent_cap` | Maximum number of recent directories to keep. |
| `worktree_root` | Parent directory for worktrees created by `shirei branch`. |
| `worktree_exclude_prefixes` | Paths ignored by `shirei worktrees scan`. |
| `run_install_on_create` | Run a detected install/build command when a worktree is created. |

Override paths with environment variables:

| Variable | Description |
| --- | --- |
| `SHIREI_CONFIG` | Override the config file path. |
| `SHIREI_STATE_DIR` | Override the state directory. |

## Worktree Setup Commands

When `run_install_on_create` is `true`, a new worktree gets a detached tmux
window or an unfocused Herdr tab named `install`. shirei runs the first matching
command:

1. `Makefile` with an `install:` target: `make install`
2. `package.json`: package-manager-specific `install`
3. `Cargo.toml`: `cargo build`

For `package.json`, the package manager is selected from the lockfile:

| Lockfile | Command |
| --- | --- |
| `bun.lockb` or `bun.lock` | `bun install` |
| `pnpm-lock.yaml` | `pnpm install` |
| `yarn.lock` | `yarn install` |
| `package-lock.json` | `npm install` |
| none | `npm install` |

## State

State is stored as JSON under:

```text
~/.local/share/shirei/state.json
```

It tracks:

- recent directories
- registered worktrees
- stack branch metadata (`branch -> base`)
- transient cascade rebase progress

Worktrees created by shirei live under:

```text
<worktree_root>/<repo-slug>/<branch-slug>
```

## Standalone PRs and MRs

`shirei pr create` requires a clean worktree, pushes the current branch to `origin` or the first configured remote, and creates or reuses an open GitHub PR or GitLab MR. The target defaults to the selected remote's HEAD branch; pass `--base` when that ref is unavailable or another target is intended.

Use `--title` and either `--body` or `--body-file` to override commit-derived review text. `--draft` creates a new review as a draft. Shirei reuses an open review that already targets the selected base; reviews targeting other branches are left untouched.

Run `shirei pr create --dry-run` to validate and print the planned branch, commit, remote, push, and review without mutating Git or contacting `gh` or `glab`. The terminal command keeps title and body optional. In Pi, `shirei_pr_create` requires the agent to inspect the diff and commits and provide a polished title and description. It always previews that authored text, requires confirmation, and rejects the operation if the confirmed branch, commit, or remote changes before creation.

## Stacked PRs

`shirei stack branch <name> --base <branch>` creates or reuses a branch worktree, opens its session, and records the branch's review base. Stack metadata is separate from worktree metadata, so `shirei worktrees scan` does not erase it.

`shirei stack rebase --branch <leaf>` validates every registered branch worktree and snapshots its branch head and merge-base OIDs before changing any branch. It then runs `git rebase --onto <new-parent-oid> <old-parent-oid>` inside each branch's own worktree from root to leaf, preventing rewritten parent commits from being replayed into children. Dry-run prints the worktree and OID boundaries for every step. The core CLI never asks an LLM to resolve conflicts; it stops with a conflict report and persists OID-aware progress so you can run `shirei stack rebase --continue` or `shirei stack rebase --abort`.

`shirei stack submit --branch <leaf>` pushes each stack branch, creates or updates GitHub PRs with `gh` or GitLab MRs with `glab`, and updates review descriptions with a managed previous/next block. Shirei only appends or replaces text between these markers:

```markdown
<!-- shirei-stack-links:start -->
...
<!-- shirei-stack-links:end -->
```

User-authored PR description text outside that block is preserved exactly. If Shirei cannot safely replace the managed block, it fails instead of rewriting the whole description.

## Forge CLI Contracts

All GitHub and GitLab subprocess interaction is isolated in `src/forge.rs`. Commands receive an explicit repository target and request JSON output with explicit fields whenever the CLI supports it. Review creation itself has no JSON output mode in `gh` or `glab`, so Shirei accepts only the emitted review URL and immediately resolves review metadata through a JSON command.

`tests/forge_cli_contract.rs` runs Shirei against fake `gh` and `glab` executables. The tests lock the required version checks, repository flags, JSON fields, noninteractive behavior, and the absence of GitLab's implicit push flags.

## Unfinished Worktrees

`shirei worktrees unfinished` checks tracked, live worktrees. A worktree is
potentially unfinished when it has uncommitted files, its HEAD is not merged
into a locally known remote default branch, its merge state cannot be
established, its directory is missing, or Git inspection fails. Detached
worktrees are reported unless their HEAD is clean and merged.

For named branches, shirei asks `gh` and `glab` for active and merged reviews
across the repository's configured remotes. Worktrees with an open or merged PR
or MR are omitted; closed, unmerged reviews remain candidates. A worktree with
local commits newer than its review remains in the report with `review=stale`.
If no forge CLI can verify a remote, it remains with `review=unknown`
rather than being treated as review-free. Pull requests targeting a fork's
upstream repository are discoverable only when that target repository is a
configured remote. Run `shirei worktrees scan` first to include worktrees
created outside shirei.
