import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Box, Text } from "@earendil-works/pi-tui";

const REPORT_TYPE = "shirei-command-report";

interface ShireiReportData {
	title: string;
	lines: string[];
	success: boolean;
}

export default function (pi: ExtensionAPI) {
	pi.registerEntryRenderer<ShireiReportData>(REPORT_TYPE, (entry, _options, theme) => {
		const data = entry.data;
		if (!data) {
			return new Text(theme.fg("error", "Shirei report is unavailable"), 0, 0);
		}

		const box = new Box(1, 1, (text) => theme.bg("customMessageBg", text));
		const color = data.success ? "accent" : "error";
		box.addChild(new Text(theme.bold(theme.fg(color, data.title)), 0, 0));
		box.addChild(new Text(data.lines.join("\n"), 0, 0));
		return box;
	});

	pi.registerCommand("shirei", {
		description: "Run non-interactive Shirei commands",
		getArgumentCompletions: (prefix) => {
			const values = [
				"branch",
				"recent",
				"sessions",
				"worktrees",
				"worktrees unfinished",
				"worktrees scan",
			];
			const matches = values.filter((value) => value.startsWith(prefix.trimStart()));
			return matches.length > 0 ? matches.map((value) => ({ value, label: value })) : null;
		},
		handler: async (args, ctx) => {
			const parts = args.trim().split(/\s+/).filter(Boolean);
			let commandArgs: string[];

			if (parts[0] === "branch") {
				if (!process.env.TMUX && !process.env.HERDR_ENV) {
					ctx.ui.notify("/shirei branch requires Pi to be running inside tmux or Herdr", "warning");
					return;
				}
				if (parts.length !== 2 && !(parts.length === 4 && (parts[2] === "--base" || parts[2] === "-b"))) {
					ctx.ui.notify("Usage: /shirei branch <name> [--base <ref>]", "warning");
					return;
				}

				const branch = parts[1];
				const branchCheck = await pi.exec("git", ["check-ref-format", "--branch", branch], { cwd: ctx.cwd });
				if (branchCheck.code !== 0) {
					ctx.ui.notify(branchCheck.stderr.trim() || `Invalid branch name: ${branch}`, "error");
					return;
				}

				const base = parts.length === 4 ? parts[3] : "HEAD";
				const baseCheck = await pi.exec("git", ["rev-parse", "--verify", `${base}^{commit}`], { cwd: ctx.cwd });
				if (baseCheck.code !== 0) {
					ctx.ui.notify(baseCheck.stderr.trim() || `Unknown base ref: ${base}`, "error");
					return;
				}
				commandArgs = ["branch", branch, "--base", base];
			} else if (parts.length === 1 && ["recent", "sessions"].includes(parts[0])) {
				commandArgs = parts;
			} else if (parts[0] === "worktrees" && parts[1] === "prune") {
				ctx.ui.notify("shirei worktrees prune is interactive; run it in a terminal", "warning");
				return;
			} else if (
				parts[0] === "worktrees" &&
				(parts.length === 1 || (parts.length === 2 && ["unfinished", "scan"].includes(parts[1])))
			) {
				commandArgs = parts;
			} else {
				ctx.ui.notify(
					"Usage: /shirei branch <name> [--base <ref>] | recent | sessions | worktrees [unfinished|scan]",
					"warning",
				);
				return;
			}

			await ctx.waitForIdle();
			ctx.ui.setStatus("shirei-command", `Running shirei ${commandArgs.join(" ")}…`);
			try {
				const result = await pi.exec("shirei", commandArgs, { cwd: ctx.cwd, timeout: 180_000 });
				const lines = [
					...result.stdout.trim().split("\n").filter(Boolean),
					...result.stderr.trim().split("\n").filter(Boolean).map((line) => `stderr: ${line}`),
				];
				if (result.killed) {
					lines.push("Command timed out or was terminated.");
				}
				const success = result.code === 0 && !result.killed;
				pi.appendEntry<ShireiReportData>(REPORT_TYPE, {
					title: `shirei ${commandArgs.join(" ")}`,
					lines: lines.length > 0 ? lines : [success ? "Completed." : `Exited with status ${result.code}.`],
					success,
				});
			} catch (error) {
				ctx.ui.notify(`Unable to run shirei: ${error instanceof Error ? error.message : String(error)}`, "error");
			} finally {
				ctx.ui.setStatus("shirei-command", undefined);
			}
		},
	});
}
