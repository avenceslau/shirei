import { randomUUID } from "node:crypto";
import { watch, type FSWatcher } from "node:fs";
import { access, mkdir, readFile, readdir, realpath, rename, unlink, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Box, Text } from "@earendil-works/pi-tui";
import { Type } from "typebox";

const REPORT_TYPE = "shirei-command-report";

interface ShireiReportData {
	title: string;
	lines: string[];
	success: boolean;
}

interface ShireiHandoff {
	target: string;
	repository: string;
	branch: string;
	prompt: string;
	createdAt: number;
}

export default function (pi: ExtensionAPI) {
	let handoffWatcher: FSWatcher | undefined;

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

	pi.on("session_start", async (_event, ctx) => {
		handoffWatcher?.close();
		handoffWatcher = undefined;

		const rootResult = await pi.exec("git", ["-C", ctx.cwd, "rev-parse", "--show-toplevel"], { timeout: 10_000 });
		if (rootResult.code !== 0) return;
		const target = await realpath(rootResult.stdout.trim()).catch(() => resolve(rootResult.stdout.trim()));

		const stateRoot = process.env.SHIREI_STATE_DIR
			? process.env.SHIREI_STATE_DIR
			: process.env.XDG_DATA_HOME
				? join(process.env.XDG_DATA_HOME, "shirei")
				: join(homedir(), ".local", "share", "shirei");
		const directory = join(stateRoot, "handoffs");
		await mkdir(directory, { recursive: true, mode: 0o700 });
		let consuming = false;
		const consume = async () => {
			if (consuming) return;
			consuming = true;
			try {
				const files = (await readdir(directory)).filter((file) => file.endsWith(".json")).sort();
				for (const file of files) {
					const path = join(directory, file);
					let handoff: ShireiHandoff;
					try {
						handoff = JSON.parse(await readFile(path, "utf8")) as ShireiHandoff;
					} catch {
						continue;
					}
					if (
						typeof handoff.target !== "string" ||
						typeof handoff.prompt !== "string" ||
						handoff.prompt.trim().length === 0
					) {
						continue;
					}
					const handoffTarget = await realpath(handoff.target).catch(() => resolve(handoff.target));
					if (handoffTarget !== target) continue;

					const claimed = `${path}.claimed-${process.pid}`;
					try {
						await rename(path, claimed);
					} catch {
						continue;
					}

					try {
						if (ctx.isIdle()) {
							pi.sendUserMessage(handoff.prompt);
						} else {
							pi.sendUserMessage(handoff.prompt, { deliverAs: "followUp" });
						}
						await unlink(claimed);
						ctx.ui.notify(`Received handoff for ${handoff.branch}`, "info");
					} catch (error) {
						await rename(claimed, path).catch(() => undefined);
						console.error("Unable to deliver Shirei handoff:", error);
					}
				}
			} finally {
				consuming = false;
			}
		};

		handoffWatcher = watch(directory, () => void consume());
		await consume();
	});

	pi.on("session_shutdown", () => {
		handoffWatcher?.close();
		handoffWatcher = undefined;
	});

	pi.registerTool({
		name: "shirei_handoff",
		label: "Shirei Handoff",
		description:
			"Create or reuse a Shirei branch worktree and send an editable, agent-authored first prompt to Pi in that session. The repository must be an existing path. Uncommitted changes are not transferred.",
		promptSnippet: "Hand work to Pi in another Shirei branch session",
		promptGuidelines: [
			"Use shirei_handoff only when the user explicitly asks to delegate work to another Shirei/Pi session.",
			"Before calling shirei_handoff, resolve the target repository path and provide a self-contained first prompt for the destination agent.",
		],
		parameters: Type.Object({
			repository: Type.String({ description: "Absolute or current-directory-relative path to the target repository" }),
			branch: Type.String({ description: "Destination branch to create or reuse" }),
			base: Type.String({ description: "Commit or branch from which to create a new destination branch" }),
			prompt: Type.String({ description: "Self-contained first prompt authored for the destination Pi agent" }),
		}),
		async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
			if (ctx.mode !== "tui") {
				throw new Error("shirei_handoff requires Pi interactive mode so the prompt can be reviewed");
			}

			const repositoryInput = params.repository.startsWith("~/")
				? join(homedir(), params.repository.slice(2))
				: params.repository === "~"
					? homedir()
					: isAbsolute(params.repository)
						? params.repository
						: resolve(ctx.cwd, params.repository);
			const rootResult = await pi.exec("git", ["-C", repositoryInput, "rev-parse", "--show-toplevel"], {
				timeout: 10_000,
			});
			if (rootResult.code !== 0) {
				throw new Error(rootResult.stderr.trim() || `${repositoryInput} is not inside a Git repository`);
			}
			const repository = await realpath(rootResult.stdout.trim()).catch(() => resolve(rootResult.stdout.trim()));
			const branch = params.branch.trim();
			const base = params.base.trim();
			if (!branch || !base) {
				throw new Error("A destination branch and base ref are required");
			}

			const branchCheck = await pi.exec("git", ["check-ref-format", "--branch", branch], {
				cwd: repository,
				timeout: 10_000,
			});
			if (branchCheck.code !== 0) {
				throw new Error(branchCheck.stderr.trim() || `Invalid branch name: ${branch}`);
			}
			const baseCheck = await pi.exec("git", ["rev-parse", "--verify", `${base}^{commit}`], {
				cwd: repository,
				timeout: 10_000,
			});
			if (baseCheck.code !== 0) {
				throw new Error(baseCheck.stderr.trim() || `Unknown base ref: ${base}`);
			}

			const editedPrompt = await ctx.ui.editor("Edit Shirei handoff prompt", params.prompt.trim());
			if (editedPrompt === undefined || editedPrompt.trim().length === 0) {
				return {
					content: [{ type: "text" as const, text: "Shirei handoff cancelled." }],
					details: { cancelled: true },
				};
			}
			const confirmed = await ctx.ui.confirm(
				"Send Shirei handoff?",
				`${repository}\nbranch: ${branch}\nbase: ${base}\n\nThe current session will remain open.`,
			);
			if (!confirmed) {
				return {
					content: [{ type: "text" as const, text: "Shirei handoff cancelled." }],
					details: { cancelled: true },
				};
			}

			const localRefs = await pi.exec("git", ["for-each-ref", "--format=%(refname:short)", "refs/heads"], {
				cwd: repository,
				timeout: 10_000,
			});
			if (localRefs.code !== 0) {
				throw new Error(localRefs.stderr.trim() || "Unable to list local branches");
			}
			const localBranches = new Set(
				localRefs.stdout
					.split("\n")
					.map((value) => value.trim())
					.filter(Boolean),
			);
			let localBranch = branch;
			if (!localBranches.has(branch)) {
				const remoteRefs = await pi.exec("git", ["for-each-ref", "--format=%(refname:short)", "refs/remotes"], {
					cwd: repository,
					timeout: 10_000,
				});
				if (remoteRefs.code !== 0) {
					throw new Error(remoteRefs.stderr.trim() || "Unable to list remote branches");
				}
				for (const remote of remoteRefs.stdout
					.split("\n")
					.map((value) => value.trim())
					.filter(Boolean)) {
					if (remote.endsWith("/HEAD")) continue;
					const separator = remote.indexOf("/");
					if (separator < 0) continue;
					const candidate = remote.slice(separator + 1);
					if (remote === branch && !localBranches.has(candidate)) {
						localBranch = candidate;
						break;
					}
				}
			}

			const backend = process.env.HERDR_WORKSPACE_ID ? "herdr" : process.env.TMUX ? "tmux" : undefined;
			if (!backend) {
				throw new Error("shirei_handoff requires Pi to be running inside tmux or Herdr");
			}
			let sourceSession: string | undefined;
			if (backend === "tmux") {
				const current = await pi.exec("tmux", ["display-message", "-p", "#{session_name}"], { timeout: 10_000 });
				if (current.code !== 0) {
					throw new Error(current.stderr.trim() || "Unable to determine the current tmux session");
				}
				sourceSession = current.stdout.trim();
			} else {
				sourceSession = process.env.HERDR_WORKSPACE_ID;
			}

			let branchResult;
			try {
				branchResult = await pi.exec("shirei", ["branch", branch, "--base", base], {
					cwd: repository,
					timeout: 180_000,
				});
			} finally {
				if (sourceSession) {
					if (backend === "tmux") {
						await pi.exec("tmux", ["switch-client", "-t", sourceSession], { timeout: 10_000 }).catch(() => undefined);
					} else {
						await pi.exec("herdr", ["workspace", "focus", sourceSession], { timeout: 10_000 }).catch(() => undefined);
					}
				}
			}
			if (branchResult.code !== 0 || branchResult.killed) {
				throw new Error(branchResult.stderr.trim() || `shirei branch exited with status ${branchResult.code}`);
			}

			const worktreeList = await pi.exec("git", ["worktree", "list", "--porcelain"], {
				cwd: repository,
				timeout: 10_000,
			});
			if (worktreeList.code !== 0) {
				throw new Error(worktreeList.stderr.trim() || "Unable to locate the destination worktree");
			}
			let candidatePath: string | undefined;
			let candidateBranch: string | undefined;
			let targetPath: string | undefined;
			for (const line of `${worktreeList.stdout}\n`.split("\n")) {
				if (line.startsWith("worktree ")) {
					candidatePath = line.slice("worktree ".length).trim();
				} else if (line.startsWith("branch ")) {
					candidateBranch = line
						.slice("branch ".length)
						.trim()
						.replace(/^refs\/heads\//, "");
				} else if (line === "") {
					if (candidatePath && candidateBranch === localBranch) {
						targetPath = candidatePath;
						break;
					}
					candidatePath = undefined;
					candidateBranch = undefined;
				}
			}
			if (!targetPath) {
				throw new Error(`Shirei completed but no worktree for ${localBranch} was found`);
			}
			const target = await realpath(targetPath).catch(() => resolve(targetPath));

			const stateRoot = process.env.SHIREI_STATE_DIR
				? process.env.SHIREI_STATE_DIR
				: process.env.XDG_DATA_HOME
					? join(process.env.XDG_DATA_HOME, "shirei")
					: join(homedir(), ".local", "share", "shirei");
			const directory = join(stateRoot, "handoffs");
			await mkdir(directory, { recursive: true, mode: 0o700 });
			const id = `${Date.now()}-${randomUUID()}`;
			const queuePath = join(directory, `${id}.json`);
			const temporaryPath = join(directory, `.${id}.tmp`);
			const handoff: ShireiHandoff = {
				target,
				repository,
				branch: localBranch,
				prompt: editedPrompt.trim(),
				createdAt: Date.now(),
			};
			await writeFile(temporaryPath, JSON.stringify(handoff), { encoding: "utf8", mode: 0o600 });
			await rename(temporaryPath, queuePath);

			let consumedByExisting = false;
			const existingDeadline = Date.now() + 1_000;
			while (Date.now() < existingDeadline) {
				try {
					await access(queuePath);
				} catch {
					consumedByExisting = true;
					break;
				}
				await new Promise((done) => setTimeout(done, 100));
			}
			if (consumedByExisting) {
				return {
					content: [
						{
							type: "text" as const,
							text: `Delivered the handoff to the existing Pi session in ${target}.`,
						},
					],
					details: { repository, branch: localBranch, base, target, startedPi: false, pending: false },
				};
			}

			let launchError: string | undefined;
			if (backend === "tmux") {
				const sessions = await pi.exec("tmux", ["list-sessions", "-F", "#{session_name}\t#{session_path}"], {
					timeout: 10_000,
				});
				const destination = sessions.stdout
					.split("\n")
					.map((line) => line.split("\t"))
					.find((parts) => parts.length >= 2 && resolve(parts.slice(1).join("\t")) === target)?.[0];
				if (!destination) {
					launchError = `Unable to find the tmux session for ${target}`;
				} else {
					const launched = await pi.exec(
						"tmux",
						["new-window", "-d", "-t", destination, "-n", "pi", "-c", target, "pi"],
						{ timeout: 10_000 },
					);
					if (launched.code !== 0) launchError = launched.stderr.trim() || "Unable to start Pi in tmux";
				}
			} else {
				const snapshot = await pi.exec("herdr", ["api", "snapshot"], { timeout: 10_000 });
				if (snapshot.code !== 0) {
					launchError = snapshot.stderr.trim() || "Unable to read Herdr workspaces";
				} else {
					const value = JSON.parse(snapshot.stdout) as {
						result?: {
							snapshot?: {
								panes?: Array<{ workspace_id?: string; cwd?: string }>;
							};
						};
					};
					const workspace = value.result?.snapshot?.panes?.find(
						(pane) => pane.cwd && resolve(pane.cwd) === target,
					)?.workspace_id;
					if (!workspace) {
						launchError = `Unable to find the Herdr workspace for ${target}`;
					} else {
						const tab = await pi.exec(
							"herdr",
							["tab", "create", "--workspace", workspace, "--cwd", target, "--label", "pi", "--no-focus"],
							{ timeout: 10_000 },
						);
						if (tab.code !== 0) {
							launchError = tab.stderr.trim() || "Unable to create a Herdr tab for Pi";
						} else {
							const tabValue = JSON.parse(tab.stdout) as { result?: { root_pane?: { pane_id?: string } } };
							const pane = tabValue.result?.root_pane?.pane_id;
							if (!pane) {
								launchError = "Herdr returned no pane for the new Pi tab";
							} else {
								const launched = await pi.exec("herdr", ["pane", "run", pane, "pi"], { timeout: 10_000 });
								if (launched.code !== 0) launchError = launched.stderr.trim() || "Unable to start Pi in Herdr";
							}
						}
					}
				}
			}

			if (launchError) {
				await unlink(queuePath).catch(() => undefined);
				throw new Error(launchError);
			}

			let delivered = false;
			const deliveryDeadline = Date.now() + 5_000;
			while (Date.now() < deliveryDeadline) {
				try {
					await access(queuePath);
				} catch {
					delivered = true;
					break;
				}
				await new Promise((done) => setTimeout(done, 100));
			}
			return {
				content: [
					{
						type: "text" as const,
						text: delivered
							? `Started Pi and delivered the handoff in ${target}.`
							: `Started Pi in ${target}; the handoff is queued and may be waiting for project trust or extension startup.`,
					},
				],
				details: { repository, branch: localBranch, base, target, startedPi: true, pending: !delivered },
			};
		},
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
