# AGENTS

shirei is a single-crate Rust CLI that drives `tmux`, `herdr`, or `rex`, plus `git` and `fzf`.
No daemon, no database, no network. State is one JSON file under
`~/.local/share/shirei/`.

## Layout

- `src/main.rs` — clap dispatch + the verbs (`pick`, `branch`, `recent`,
  `sessions`, `worktrees [prune]`)
- `src/picker.rs` — builds the sectioned source list, runs fzf, returns
  the chosen row index
- `src/tmux.rs` — `list-sessions`, `new-session -d`,
  `switch-client` / `attach-session`, session-name sanitization
- `src/herdr.rs` — workspace listing, creation, focus, close, and setup tabs
- `src/rex.rs` — session listing, creation, app selection, close, and setup windows
- `src/session.rs` — backend-neutral session model and backend dispatch
- `src/git.rs` — `repo_root`, `list_branches`, `create_worktree`
  (shells out to `git`)
- `src/state.rs` — JSON state load/save, MRU touch, worktree dedup,
  dead-worktree prune
- `src/config.rs` — TOML config load + tilde expansion
- `src/stack.rs` — stack graph, rebase/submit plans, managed stack links block
- `src/forge.rs` — every `gh` and `glab` call
- `src/review.rs` — review title/body rules (ticket prefix, public denylist,
  word budget, hand-written stack sections)
- `build.rs` — embeds the git SHA shown by `--version` and `capabilities`

## Conventions

- All external IO goes through `std::process::Command`; no async, no
  reqwest, no sqlite.
- Tmux session names go through `tmux::sanitize_name` (strips `.`, `:`,
  `/`, etc.).
- The picker source order is the single source of truth for ordering;
  do not duplicate it elsewhere.
- Don't add daemon-style features. If you reach for tokio or axum here,
  stop and reconsider.

## Tests

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Integration tests in `tests/` run the built binary against temp git repos with
`SHIREI_STATE_DIR` and `SHIREI_CONFIG` pointing at temp paths, and fake `gh`,
`glab`, `git push`, `tmux`, and `herdr` executables on `PATH`. Never point a test
at the real state or config. Backend JSON contracts have unit coverage.
Interactive flows (picker and branch picker) are smoke-tested by hand in the
configured terminal backend.
