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

The unfinished-worktree report also uses the CLI for your repository host:

- `gh` for GitHub pull requests
- `glab` for GitLab merge requests

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

## tmux Binding

Add a binding to your tmux config:

```tmux
bind-key -r f run-shell "tmux neww shirei"
```

Then press `prefix + f` to open the picker in a new tmux window.

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
```

Then press your Herdr prefix followed by `f`. To use `Ctrl-a` as the prefix,
add this above the command binding:

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

## Picker Order

Rows are grouped in this order:

1. `SESSION`: live tmux sessions or Herdr workspaces. Tmux sessions are sorted by recent activity; Herdr preserves workspace order.
2. `RECENT`: directories opened recently through shirei.
3. `WORKTREE`: tracked git worktrees.
4. `ROOT`: directories discovered under configured roots.

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

Worktrees created by shirei live under:

```text
<worktree_root>/<repo-slug>/<branch-slug>
```

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
