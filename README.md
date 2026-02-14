# Shirei

Local daemon + CLI to supervise agent jobs (OpenCode) in isolated git worktrees and watch GitHub/GitLab PR/MR comments for commands.

## Quick start

Install bundled binaries from crates.io:

```bash
cargo install shirei
```

This installs `shirei`, `shirei-daemon`, and `shirei-tui` together.

```bash
# required for GitHub provider (unless using mock):
export SHIREI_GITHUB_TOKEN=...
# optional: for GitLab
export SHIREI_GITLAB_TOKEN=...
export SHIREI_GITLAB_HOST=gitlab.com

cargo run -p shirei-daemon
```

In another terminal:

```bash
TOKEN=$(cat ~/.local/share/shirei/token)
cargo run -p shirei -- --token "$TOKEN" health
cargo run -p shirei -- --token "$TOKEN" doctor
cargo run -p shirei -- --token "$TOKEN" doctor --project-id <project-id>
cargo run -p shirei -- --token "$TOKEN" project-discover .

# list jobs for a project
cargo run -p shirei -- --token "$TOKEN" jobs "$PROJECT_ID"
# attach opencode to an existing job session
cargo run -p shirei -- --token "$TOKEN" jobs <job-id> attach

# inspect or stop persistent attach backend
cargo run -p shirei -- --token "$TOKEN" jobs <job-id> attach-status
cargo run -p shirei -- --token "$TOKEN" jobs <job-id> attach-stop

# install global OpenCode slash commands
cargo run -p shirei -- setup opencode
```

`doctor` checks daemon reachability, token presence, provider readiness (`SHIREI_GITHUB_TOKEN` / GitLab vars), and project remote suitability for auto PR/watch.

## CLI config

Optional config path: `~/.config/shirei/config.toml` (or override with `SHIREI_CONFIG`).

```toml
[editor]
command = "code --reuse-window {path}"
```

`{path}` is replaced with the job worktree path (or job cwd when no worktree is attached).

## Polling-first PR/MR watching

Create a watch:

- GitHub: `github:owner/repo`
- GitLab: `gitlab:group/subgroup/project`

```bash
cargo run -p shirei -- --token "$TOKEN" watch-start github:owner/repo 123 --retention-days 7
```

Commands recognized in PR/MR comments:

- `/shirei fix <description>`
- `/shirei stop`
- `/shirei respond`
- `/shirei approve`
- `/shirei request-changes <feedback>`

Manual jobs now enter a review loop when the agent run exits: Shirei marks them `AWAITING_REVIEW`, auto-creates a draft PR/MR when it can (and attaches a watch), and waits for `/shirei approve` or `/shirei request-changes ...`.

Open a job branch/worktree in your configured editor:

```bash
shirei --token "$TOKEN" jobs <job-id> edit
```

## TUI

Run the terminal UI against a running daemon:

```bash
cargo run -p shirei-tui -- --token "$TOKEN" --discover-path .
```

Useful keys: `?` show actions help, `d` discover project, `n` create job, `y` approve, `c` request changes (rerun), `g` grant permission once, `G` grant permission always, `x` reject permission, `1-9` answer first pending question option, `0` reject question, `a` ack, `u` unack, `k` kill, `r` refresh, `m` maximize logs, `t` attach interactive, `e` edit interactive, `T` attach background, `E` edit background, `o` open linked PR in browser. Logs are colorized by source (`agent`, `output`, `stderr`).

`t` and `e` run attach/edit interactively in the current terminal and return to TUI on exit. `t` now attaches to a persistent per-job OpenCode backend, so exiting OpenCode (`/q`) detaches while keeping the backend running for later reattach. Use `T` / `E` for background launch.

## OpenCode setup

Install global OpenCode command files:

```bash
shirei setup opencode
```

This writes files to `~/.config/opencode/commands` for:

- `/shirei-attach` (runs `shirei jobs <job-id> attach`)
- `/shirei-edit` (runs `shirei jobs <job-id> edit`)

It also installs a global skill at `~/.config/opencode/skills/shirei-workflow/SKILL.md` so OpenCode agents can load Shirei-specific operational guidance.

## Configuration

Shirei daemon loads config from `~/.local/share/shirei/config.json` by default (or from
`$SHIREI_STATE_DIR/config.json` when `SHIREI_STATE_DIR` is set).

Current schema:

```json
{
  "commands": {
    "opencode": ["opencode"]
  }
}
```

- `commands.opencode` may be a string (`"opencode"`) or an argv array.
- If omitted, default is `opencode`.
- `SHIREI_OPENCODE_CMD` still works and overrides config for compatibility.

## Testing (mock opencode)

Tests may set `SHIREI_OPENCODE_CMD` or write `config.json` in the temp state dir.
