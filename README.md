# shirei

`shirei` is a small tmux session switcher for people who jump between many
projects and git worktrees.

It gives you one `fzf` picker for:

- live tmux sessions
- recently opened directories
- git worktrees created or discovered by shirei
- project roots under your configured search paths

Selecting a row switches to the existing tmux session when one exists, or starts
a new session in that directory.

## Requirements

`shirei` shells out to existing tools. Install these first:

- `tmux`
- `git`
- `fzf`

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

## tmux Binding

Add a binding to your tmux config:

```tmux
bind-key -r f run-shell "tmux neww shirei"
```

Then press `prefix + f` to open the picker in a new tmux window.

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

Remove stale tracked worktrees whose directories no longer exist:

```bash
shirei worktrees prune
```

## Commands

| Command | Description |
| --- | --- |
| `shirei` | Open the picker. Same as `shirei pick`. |
| `shirei pick` | Open the picker explicitly. |
| `shirei branch [name]` | Create or attach to a git worktree, register it, and open a tmux session there. |
| `shirei branch [name] --base <ref>` | Use `<ref>` as the start point for a new branch. |
| `shirei recent` | Print recent directories. |
| `shirei sessions` | Print live tmux sessions. |
| `shirei worktrees` | List tracked worktrees. |
| `shirei worktrees scan` | Rebuild tracked worktrees from `git worktree list`. |
| `shirei worktrees prune` | Drop tracked worktrees whose paths no longer exist. |

## Picker Order

Rows are grouped in this order:

1. `TMUX`: live tmux sessions, sorted by recent activity.
2. `RECENT`: directories opened recently through shirei.
3. `WORKTREE`: tracked git worktrees.
4. `ROOT`: directories discovered under configured roots.

Earlier groups win. If a live tmux session already represents a directory, that
directory is hidden from later groups so you do not see duplicate entries.

## Configuration

Configuration is optional. By default, shirei reads:

```text
~/.config/shirei/config.toml
```

Example:

```toml
roots = ["~/projects/*", "~", "~/.config"]
recent_cap = 20
worktree_root = "~/.local/share/shirei/worktrees"
worktree_exclude_prefixes = ["~/Library/Caches"]
run_install_on_create = true
```

Settings:

| Setting | Description |
| --- | --- |
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
window named `install`. shirei runs the first matching command:

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
