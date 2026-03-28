# Shirei

Local daemon + CLI to supervise OpenCode sessions in isolated git worktrees.

## Quick start

Install bundled binaries from crates.io:

```bash
cargo install shirei
```

This installs `shirei` and `shirei-daemon`.

The CLI-first flow expects `fzf` and `tmux` to be available on your machine.

Run `shirei` with no subcommand to open the CLI session picker:

- loads active Shirei sessions into `fzf`,
- shows which sessions need `INPUT` or `REVIEW`,
- opens the selected session inside `tmux` and attaches OpenCode there.

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

# list active sessions
cargo run -p shirei -- --token "$TOKEN" sessions
# attach opencode to an existing job session
cargo run -p shirei -- --token "$TOKEN" jobs <job-id> attach
# create a fresh branch worktree or attach an existing branch
cargo run -p shirei -- --token "$TOKEN" branch feature/my-change
# open an fzf branch picker (includes local + remote branches)
cargo run -p shirei -- --token "$TOKEN" branch

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

When a background session finishes or starts waiting for user input, the daemon sends a desktop notification.

Open a job branch/worktree in your configured editor:

```bash
shirei --token "$TOKEN" jobs <job-id> edit
```

## Session flow

- `shirei` opens an `fzf` picker for active sessions.
- `shirei branch` opens an `fzf` picker of local and remote branches, creates or reuses the matching worktree, and opens OpenCode in tmux.
- `shirei branch <branch-name>` attaches that existing local/remote branch when it exists, or creates a new branch worktree when it does not.
- Session rows are badged as `RUNNING`, `INPUT`, `REVIEW`, or `ERROR`.
- `shirei jobs <job-id> attach` still attaches directly when you already know the session id.

## OpenCode setup

Install global OpenCode command files:

```bash
shirei setup opencode
```

This writes files to `~/.config/opencode/commands` for:

- `/shirei-attach` (runs `shirei jobs <job-id> attach`)
- `/shirei-edit` (runs `shirei jobs <job-id> edit`)

It also installs a global skill at `~/.config/opencode/skills/shirei-workflow/SKILL.md` so OpenCode agents can load Shirei-specific operational guidance.

`shirei setup opencode` also installs a primary OpenCode agent at `~/.config/opencode/agent/shirei.md`.
Shirei job sessions run `opencode` with `--agent shirei` by default.

## Configuration

Shirei daemon loads config from `~/.local/share/shirei/config.json` by default (or from
`$SHIREI_STATE_DIR/config.json` when `SHIREI_STATE_DIR` is set).

Current schema:

```json
{
  "commands": {
    "opencode": ["opencode"],
    "bootstrap": ["bash", "-lc", "pnpm install"]
  }
}
```

- `commands.opencode` may be a string (`"opencode"`) or an argv array.
- If omitted, default is `opencode`.
- `commands.bootstrap` is optional and runs inside new branch worktrees before tmux/OpenCode opens.
- `commands.bootstrap` may also be a string or argv array; use an argv array like `["bash", "-lc", "pnpm install"]` for shell pipelines.
- `commands.bootstrap` supports `{path}` placeholders, although it already runs with the worktree as its current directory.
- `SHIREI_OPENCODE_CMD` still works and overrides config for compatibility.

## Testing (mock opencode)

Tests may set `SHIREI_OPENCODE_CMD` or write `config.json` in the temp state dir.
