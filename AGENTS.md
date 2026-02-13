# AGENTS

This repository uses Shirei sessions to execute implementation tasks in a trackable way.

## Default Agent Loop

1. Confirm daemon health.
2. Ensure repo is discovered as a project.
3. Spawn a named job with a precise prompt.
4. Poll jobs and inspect logs.
5. Validate with `cargo fmt` and `cargo test`.
6. Acknowledge completed jobs; kill and retry stalled jobs.

## Commands

```bash
TOKEN="$(cat ~/.local/share/shirei/token)"
shirei --token "$TOKEN" health
shirei --token "$TOKEN" project-discover .
PROJECT_ID="$(shirei --token "$TOKEN" projects | jq -r '.projects[0].project_id')"
shirei --token "$TOKEN" spawn "$PROJECT_ID" "task-name" "task prompt"
shirei --token "$TOKEN" jobs "$PROJECT_ID"
shirei --token "$TOKEN" jobs <job-id> attach
shirei --token "$TOKEN" logs <job-id> --tail-lines 300
```

## Prompt Quality Rules

- Be explicit about scope and files.
- Include acceptance criteria and test commands.
- Prefer minimal, production-safe changes.
- Ask for a final change summary with paths.

## Session States (for TUI/ops)

- `RUNNING`: active execution
- waiting input: running but attention needed
- awaiting review: finished/error and attention needed
- completed/merged sessions should be hidden from actionable lists
