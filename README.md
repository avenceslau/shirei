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

Copy the extension again whenever you install a new `shirei`: the extension and
the binary change together (for example, the extension passes `--open` to
`shirei branch`, which older binaries reject).

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
enter LLM context. Interactive prune remains terminal-only. Only the
`/shirei stack submit` slash command requires `--dry-run`; running
`shirei stack submit` through bash works.

For a standalone review, ask Pi to use `shirei_pr_create`. The agent writes a
short title and description following the `pr-description` skill: one to three
sentences of why, a few behaviour bullets, and a line or two of testing, within
the word budget for the change size. Shirei adds everything deterministic itself
(the ticket prefix, stack links) and rejects bodies that break the rules (see
[Standalone PRs and MRs](#standalone-prs-and-mrs)). The tool previews the push
and review, then offers Create, Create as draft, or Cancel with a note; the note
goes back to the agent so it can revise. Without an interactive session, for
example in a subagent, the tool validates the review and returns it instead of
creating it, so the parent can confirm it.

For LLM-assisted cascade rebases, ask Pi to use `shirei_stack_rebase_assist`;
it confirms first, then fixes conflicts with the first available model in
`SHIREI_REBASE_MODEL_ORDER` (comma-separated `provider/id` values), or the
current model when that is unset.

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

From a script or an agent, create and register the worktree without opening a
session, print only its path, check out only some directories, and run the
setup command in the foreground:

```bash
shirei branch feature/my-change --base origin/main --no-open --print-path \
  --install=sync --sparse packages/api packages/shared
```

`--no-open` is the default when stdin is not a terminal or `PI_CODING_AGENT`
is set; `--open` forces a session. New branches never track their base, so the
first `git push -u` sets the upstream.

Register one worktree that was created with `git worktree add`:

```bash
shirei worktrees add ~/.local/share/shirei/worktrees/repo/feature-my-change
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

In Pi, use the `shirei_stack_rebase_assist` tool when you explicitly want Pi to drive the cascade rebase and fix conflicts with the model from `SHIREI_REBASE_MODEL_ORDER` or the current model.

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

Without fzf, preview and then delete clean worktrees whose branches are merged:

```bash
shirei worktrees prune --merged
shirei worktrees prune --merged --yes [PATH...]
```

A branch counts as merged when its HEAD is in a remote default branch or in a
merged PR or MR. Dirty, unmerged, detached, primary, and current-directory
worktrees are skipped with a reason. `shirei worktrees prune --missing` drops
registry entries whose directories are gone and deletes nothing on disk.

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
| `shirei branch <name> --no-open --print-path` | Create and register the worktree without a session and print its path. |
| `shirei branch <name> --sparse <dir>...` | Check out only these directories (cone-mode sparse checkout). |
| `shirei branch <name> --install=sync\|background\|none` | Run the detected setup command in the foreground, in a session window, or not at all. |
| `shirei recent` | Print recent directories. |
| `shirei sessions` | Print live sessions from the configured backend. |
| `shirei kill` | Select backend sessions or workspaces to close. |
| `shirei completions <shell>` | Generate a shell completion script. |
| `shirei worktrees` | List tracked worktrees. |
| `shirei worktrees unfinished` | List dirty, unmerged, missing, or unclassified worktrees without an active or merged PR or MR. |
| `shirei worktrees scan` | Rebuild tracked worktrees from `git worktree list`. |
| `shirei worktrees add <path>` | Register one linked worktree without a full scan. |
| `shirei worktrees prune` | Select and permanently delete worktree directories, metadata, local branches, and Shirei entries. |
| `shirei worktrees prune --merged [--yes] [path...]` | Preview, or with `--yes` delete, clean merged worktrees and their local branches. |
| `shirei worktrees prune --missing` | Remove registry entries whose paths are gone. |
| `shirei capabilities [--json]` | Print the version, git SHA, and every subcommand with its flags. |
| `shirei pr create` | Push the current branch and create or reuse its GitHub PR or GitLab MR. |
| `shirei pr create --dry-run` | Preview standalone review creation without pushing or contacting the forge. |
| `shirei pr create --remote <name>` | Push to and open the review on this remote. |
| `shirei stack branch <name> --base <branch>` | Create or attach to a worktree and record the PR/MR base branch. |
| `shirei stack track <branch> --base <branch>` | Record stack metadata for an existing branch. |
| `shirei stack untrack <branch>` | Remove stack metadata without deleting Git branches or worktrees. |
| `shirei stack list` | Print tracked stack branches and their bases, paths, reviews, and problems. |
| `shirei stack rebase --branch <leaf>` | Cascade rebase a stack from root to leaf, stopping safely on conflicts. |
| `shirei stack rebase --continue` | Continue a stopped cascade rebase after conflicts are resolved and staged. |
| `shirei stack rebase --abort` | Abort the current stopped cascade rebase. |
| `shirei stack submit --branch <leaf>` | Force-push stack branches with a lease, create/update GitHub PRs or GitLab MRs, and update the managed stack links. |

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
checkout_exclude = [".dev.vars*", ".env*"]

[review]
private_hosts = ["gitlab.corp.example"]
extra_public_denylist = []
small_max_changed_lines = 100
large_above_changed_lines = 800
large_min_top_level_dirs = 3
small_max_words = 80
medium_max_words = 200
large_max_words = 350
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
| `checkout_exclude` | gitignore-style patterns never written to new worktrees, through a non-cone sparse checkout. Useful when a sandbox refuses to write files such as `.dev.vars.example`. |
| `review.private_hosts` | Forge hosts whose reviews are private. Reviews on other hosts are checked against the public denylist. |
| `review.extra_public_denylist` | Terms added to the built-in public denylist. |
| `review.*_changed_lines`, `review.large_min_top_level_dirs` | How `pr create` sizes a change as small, medium, or large. |
| `review.*_max_words` | Prose word budgets of review bodies by change size. |

Per-repository Git config:

| Key | Description |
| --- | --- |
| `shirei.remote` | Remote for reviews when the branch has no upstream remote. |
| `shirei.ticketPrefix` | Ticket key for review titles, such as `ABC`, or `none`. Detected from recent titles when unset. |

Override paths with environment variables:

| Variable | Description |
| --- | --- |
| `SHIREI_CONFIG` | Override the config file path. |
| `SHIREI_STATE_DIR` | Override the state directory. |

## Worktree Setup Commands

When `run_install_on_create` is `true`, a new worktree gets a detached tmux
window or an unfocused Herdr tab named `install`. `--install=sync` runs the
command in the foreground instead, sends its output to stderr, and exits non-zero
when it fails (the worktree stays). `--install=none` skips it. Without a session
(`--no-open`), background setup is skipped with a note. shirei runs the first
matching command:

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
- stack branch metadata (`branch -> base`, the base commit, and review links)
- transient cascade rebase progress

Repositories are keyed by their main worktree (the parent of
`git rev-parse --git-common-dir`), so commands behave the same from any linked
worktree. Older state keyed by a linked worktree is migrated once on load.
`worktrees scan` keeps the previous entries of a repository it cannot scan.

Worktrees created by shirei live under:

```text
<worktree_root>/<repo-slug>/<branch-slug>
```

## Standalone PRs and MRs

`shirei pr create` requires a clean worktree, pushes the current branch, and creates or reuses an open GitHub PR or GitLab MR. It pushes to `--remote`, else the branch's push or upstream remote, else `git config shirei.remote`, else `origin`, else the first remote. The target defaults to the stack base for a tracked stack branch, otherwise to the remote's default branch as reported by `git ls-remote --symref` (Shirei warns when the local `refs/remotes/<remote>/HEAD` disagrees); pass `--base` for another target.

Use `--title` and either `--body` or `--body-file` to override commit-derived review text. `--draft` creates a new review as a draft. Shirei reuses an open review that already targets the selected base; reviews targeting other branches are left untouched.

Shirei produces the deterministic parts of a review and checks the rest, in the terminal and in Pi alike:

- **Stack links.** On a tracked stack branch, the body gets the managed links block (position such as `2 of 4`, previous, next, and merge order), and the same block is refreshed on every open review in the stack. A body with its own `Stack` or `Stack position` heading is rejected rather than edited, so the author learns to leave it out and no prose is removed silently.
- **Ticket prefix.** When at least half of the last 20 titles on the remote's default branch start with `KEY-NNNN:` (or `shirei.ticketPrefix` names the key), the ticket from the branch name or the commits is prepended to the title, and an all-zero placeholder such as `ABC-000` is replaced.
- **Public scrub.** On hosts outside `review.private_hosts`, a title or body containing a denylisted internal name is rejected with every hit listed.
- **Word budget.** The change against the base is small (at most 100 changed lines), large (over 800 lines or 3 or more top-level directories), or medium; bodies over 80, 200, or 350 prose words are rejected. Fenced code and the stack links block do not count.

Run `shirei pr create --dry-run` to validate and print the planned branch, commit, remote, push, final title and body, word count, and stack position without pushing or contacting `gh` or `glab`. The terminal command keeps title and body optional. In Pi, `shirei_pr_create` asks the agent for a short title and description following the `pr-description` skill, previews them, and rejects the operation if the confirmed branch, commit, or remote changes before creation.

## Stacked PRs

`shirei stack branch <name> --base <branch>` creates or reuses a branch worktree, opens its session, and records the branch's review base and the base commit it starts from. It takes the same `--no-open`, `--print-path`, `--sparse`, and `--install` flags as `shirei branch`. Stack metadata is separate from worktree metadata, so `shirei worktrees scan` does not erase it.

`shirei stack rebase --branch <leaf>` validates every registered branch worktree and snapshots its branch head and old base OIDs before changing any branch. The old base is the parent commit recorded by `stack branch`, `track`, the last rebase, or `submit` when the parent was rewritten since, so commits of a parent amended or rebased outside Shirei are not replayed; otherwise it is the merge-base. It then runs `git rebase --onto <new-parent-oid> <old-parent-oid>` inside each branch's own worktree from root to leaf, preventing rewritten parent commits from being replayed into children. Dry-run prints the worktree and OID boundaries for every step. The core CLI never asks an LLM to resolve conflicts; it stops with a conflict report and persists OID-aware progress so you can run `shirei stack rebase --continue` or `shirei stack rebase --abort`.

`shirei stack submit --branch <leaf>` force-pushes each stack branch with `--force-with-lease=<branch>:<remote commit seen before the first push>`, so rebased branches push but someone else's newer push is never overwritten. It then creates or updates GitHub PRs with `gh` or GitLab MRs with `glab` and updates review descriptions with the managed stack links block (`glab mr update --description`). Shirei only appends or replaces text between these markers:

```markdown
<!-- shirei-stack-links:start -->
...
<!-- shirei-stack-links:end -->
```

User-authored PR description text outside that block is preserved exactly. If Shirei cannot safely replace the managed block, it fails instead of rewriting the whole description.

## Forge CLI Contracts

All GitHub and GitLab subprocess interaction is isolated in `src/forge.rs`. Commands receive an explicit repository target and request JSON output with explicit fields whenever the CLI supports it. Review creation itself has no JSON output mode in `gh` or `glab`, so Shirei accepts only the emitted review URL and immediately resolves review metadata through a JSON command.

`tests/forge_cli_contract.rs` runs Shirei against fake `gh` and `glab` executables. The tests lock the required version checks, repository flags, JSON fields, noninteractive behavior, and the absence of GitLab's implicit push flags. The fake `glab` rejects any flag that glab 1.108 does not list in `glab mr create --help` or `glab mr update --help`, and one test checks the flags Shirei uses against the installed `glab` when there is one.

## Sparse Checkouts

`--sparse <dir>...` on `shirei branch` and `shirei stack branch` creates the worktree with `git worktree add --no-checkout`, runs `git sparse-checkout set --cone <dir>...`, then checks out. Top-level files and the files directly inside each parent of a listed directory are always included, as in Git's cone mode. Put `--sparse` last or repeat it, because it takes every following value. Widen the checkout later with `git sparse-checkout add <dir>` inside the worktree.

`checkout_exclude` uses non-cone patterns (`/*` followed by `!<pattern>` for each exclusion) so the listed files are never written. When both are set, Shirei writes the non-cone equivalent of the cone directories followed by the exclusions. If any checkout step fails, Shirei removes the partial directory and the branch it created, so rerunning starts fresh.

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
