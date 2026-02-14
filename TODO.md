## Shirei config

- [x] There should be a shirei config, in the config users can setup the commands that open there editor

## Shirei Jobs

- [x] shirei should have a shirei jobs <job-id> attach command that attaches opencode to a session
- [x] shirei should have a shirei jobs <job-id> edit to open the editor on a specific job branch

## Observability

- [x] daemon runtime logging is in place and useful across API traffic, runner lifecycle/retries, PR watch polling/commands, and purge passes/results.

## TUI

- [x] I want to be able to scroll look back at the logs for a given session
- [x] improve log styling with richer semantics (agent/user/tool/system) beyond stdout/stderr heuristics
- [ ] add true interactive attach that opens in a dedicated terminal window while keeping TUI non-blocking

## CLI / Daemon UX

- [x] CLI should auto-start the daemon in the background when running a command and daemon is not up
- [x] add `shirei start|stop|restart` lifecycle commands and `shirei tui` to launch the TUI
- [ ] add `shirei doctor --json` for machine-readable health/readiness checks

## Packaging

- [x] bundle `shirei-daemon` and `shirei-tui` into the published `shirei` package so one install provides all binaries

## Jobs UX

- [ ] add a way to restart an existing job from CLI/TUI/API
- [x] add in-TUI action to answer permission/input prompts for running jobs
- [ ] support mid-prompt Shirei attach from OpenCode so closing OpenCode keeps the session managed by Shirei
