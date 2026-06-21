# shirei

A tmux session manager with worktree-aware dir tracking. Replaces a
hand-rolled `tmux-sessionizer` script with:

- live tmux sessions surfaced at the top of the picker, MRU-sorted
- a persisted history of recently-opened dirs
- one-shot worktree creation that registers the worktree in the picker
- fzf as the picker UI

Requires `tmux`, `git`, and `fzf` on the PATH.

## Install

```bash
git clone https://github.com/avenceslau/shirei
cd shirei
cargo install --path .
```

This installs a single `shirei` binary into `~/.cargo/bin`.

## tmux integration

```tmux
bind-key -r f run-shell "tmux neww shirei"
```

`prefix + f` opens the picker in a new tmux window.

## Commands

- `shirei` — open the picker (same as `shirei pick`)
- `shirei branch [<name>] [-b/--base <ref>]` — create or attach a git
  worktree for `<name>`, register it, and open a tmux session inside it.
  With no argument, opens an fzf picker over local + remote branches. When
  `<name>` is a new branch, `--base` sets its start-point; if omitted, an
  fzf picker prompts for the base branch.
- `shirei recent` — print recent dirs
- `shirei sessions` — print live tmux sessions
- `shirei worktrees` — list tracked worktrees
- `shirei worktrees scan` — walk configured roots and known repos and
  rebuild the worktree registry from `git worktree list`. Run this to
  pick up worktrees created outside shirei.
- `shirei worktrees prune` — drop tracked worktrees whose path no longer exists

## Picker order

From top to bottom:

1. `TMUX` — live tmux sessions, sorted by `session_activity` descending.
   Selecting one runs `switch-client` (inside tmux) or `attach-session`.
2. `RECENT` — directories opened recently via shirei, MRU-sorted, capped
   to `recent_cap` entries.
3. `WORKTREE` — worktrees shirei has created.
4. `ROOT` — directories discovered under configured roots (default
   `~/projects/*`, `~`, `~/.config`).

A path covered by an earlier section is hidden from later sections. If a
tmux session already exists whose name matches the basename of a dir, the
dir row is hidden in favour of the session row.

## Configuration

Optional config file at `~/.config/shirei/config.toml`:

```toml
roots = ["~/projects/*", "~", "~/.config"]
recent_cap = 20
worktree_root = "~/.local/share/shirei/worktrees"
worktree_exclude_prefixes = ["~/Library/Caches"]
```

`worktree_exclude_prefixes` filters paths discovered by `shirei worktrees scan`
(default excludes tenet's cache worktrees on macOS).

Set `SHIREI_CONFIG` to override the config path. Set `SHIREI_STATE_DIR` to
override the state dir (defaults to `$XDG_DATA_HOME/shirei` or
`~/.local/share/shirei`). Config defaults to `$XDG_CONFIG_HOME/shirei/config.toml`
or `~/.config/shirei/config.toml`.

## State

State lives in `~/.local/share/shirei/state.json` and persists:

- `recent` — recent dirs (path + last-used timestamp)
- `worktrees` — worktrees created by `shirei branch`

Worktrees are created under `worktree_root` at
`<worktree_root>/<repo-slug>/<branch-slug>`.
