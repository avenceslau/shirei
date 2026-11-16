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

interface PendingStackRebase {
	repository: string;
	branch: string;
	attempts: number;
	maxAttempts: number;
	previousModel?: unknown;
	modelLabel?: string;
	currentWorktree?: string;
	currentBranch?: string;
	currentBase?: string;
	currentFiles?: string[];
}

const DEFAULT_REBASE_MODEL_ORDER = ["openai/gpt-5.6-luna", "openai/gpt-5.6-terra", "openai/gpt-5.6-sol"];

function parseConflictHeader(output: string) {
	const line = output.split("\n").find((value) => value.startsWith("status=conflict "));
	if (!line) return undefined;
	const values = new Map<string, string>();
	for (const part of line.split(/\s+/).slice(1)) {
		const separator = part.indexOf("=");
		if (separator > 0) values.set(part.slice(0, separator), part.slice(separator + 1));
	}
	const branch = values.get("branch");
	const base = values.get("base");
	const worktree = values.get("worktree");
	return branch && base && worktree ? { branch, base, worktree } : undefined;
}

async function conflictSummary(pi: ExtensionAPI, worktree: string) {
	const diff = await pi.exec("git", ["diff", "--name-only", "--diff-filter=U"], { cwd: worktree, timeout: 10_000 });
	const files = diff.stdout
		.split("\n")
		.map((line) => line.trim())
		.filter(Boolean);
	const lines: string[] = [];
	for (const file of files) {
		let text = "";
		try {
			text = await readFile(join(worktree, file), "utf8");
		} catch {
			lines.push(`- ${file}: conflict markers unavailable`);
			continue;
		}
		let start: number | undefined;
		let separator: number | undefined;
		const blocks: string[] = [];
		text.split("\n").forEach((line, index) => {
			const lineNumber = index + 1;
			if (line.startsWith("<<<<<<<")) {
				start = lineNumber;
				separator = undefined;
			} else if (line.startsWith("=======")) {
				separator = lineNumber;
			} else if (line.startsWith(">>>>>>>") && start !== undefined) {
				blocks.push(separator ? `${start}-${lineNumber} (ours ${start + 1}-${separator - 1}, theirs ${separator + 1}-${lineNumber - 1})` : `${start}-${lineNumber}`);
				start = undefined;
				separator = undefined;
			}
		});
		lines.push(`- ${file}${blocks.length ? `: ${blocks.join(", ")}` : ""}`);
	}
	return { files, text: lines.join("\n") || "- conflict files unavailable" };
}

function formatRebasePrompt(branch: string, base: string, worktree: string, conflicts: string) {
	return `Shirei is cascade rebasing branch ${branch} onto ${base} in ${worktree}.
Git stopped with conflicts.

Conflicts:
${conflicts}

Resolve the conflicts by editing files in this worktree only.
Requirements:
1. Preserve intended changes from both sides when possible.
2. Remove all conflict markers.
3. Run focused checks if practical.
4. Stage only the resolved conflict files.
5. Do not continue or abort the rebase; Shirei will run \`shirei stack rebase --continue\` after verification.`;
}

function repositoryInput(value: string | undefined, cwd: string): string {
	if (!value) {
		return cwd;
	}
	if (value === "~") {
		return homedir();
	}
	if (value.startsWith("~/")) {
		return join(homedir(), value.slice(2));
	}
	return isAbsolute(value) ? value : resolve(cwd, value);
}

export default function (pi: ExtensionAPI) {
	let handoffWatcher: FSWatcher | undefined;
	let pendingStackRebase: PendingStackRebase | undefined;

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

	pi.on("session_shutdown", async (_event, ctx) => {
		handoffWatcher?.close();
		handoffWatcher = undefined;
		if (pendingStackRebase?.previousModel) {
			try {
				await pi.setModel(pendingStackRebase.previousModel as never);
			} catch {}
		}
		pendingStackRebase = undefined;
		ctx.ui.setStatus("shirei-stack-rebase", undefined);
	});

	const selectFastestRebaseModel = async (ctx: any, order: string[]) => {
		for (const value of order) {
			const [provider, ...rest] = value.split("/");
			const id = rest.join("/");
			if (!provider || !id) continue;
			const model = ctx.modelRegistry.find(provider, id);
			if (!model) continue;
			if (await pi.setModel(model)) return { model, label: value };
		}
		return undefined;
	};

	const finishPendingStackRebase = async (ctx: any, message: string, level: "info" | "warning" | "error") => {
		if (pendingStackRebase?.previousModel) {
			try {
				await pi.setModel(pendingStackRebase.previousModel as never);
			} catch {}
		}
		pendingStackRebase = undefined;
		ctx.ui.setStatus("shirei-stack-rebase", undefined);
		ctx.ui.notify(message, level);
	};

	const handleStackRebaseOutput = async (output: string, ctx: any) => {
		const pending = pendingStackRebase;
		if (!pending) return;
		const conflict = parseConflictHeader(output);
		if (!conflict) {
			await finishPendingStackRebase(ctx, `Completed Shirei stack rebase for ${pending.branch}`, "info");
			return;
		}
		pending.attempts += 1;
		if (pending.attempts > pending.maxAttempts) {
			await finishPendingStackRebase(ctx, `Shirei stack rebase stopped after ${pending.maxAttempts} conflict attempt(s)`, "warning");
			return;
		}
		pending.currentWorktree = conflict.worktree;
		pending.currentBranch = conflict.branch;
		pending.currentBase = conflict.base;
		const summary = await conflictSummary(pi, conflict.worktree);
		pending.currentFiles = summary.files;
		ctx.ui.setStatus("shirei-stack-rebase", `Conflict on ${conflict.branch}; waiting for ${pending.modelLabel ?? "LLM"}`);
		const prompt = formatRebasePrompt(conflict.branch, conflict.base, conflict.worktree, summary.text);
		if (ctx.isIdle()) {
			pi.sendUserMessage(prompt);
		} else {
			pi.sendUserMessage(prompt, { deliverAs: "followUp" });
		}
	};

	const continuePendingStackRebase = async (ctx: any) => {
		const pending = pendingStackRebase;
		if (!pending || !ctx.isIdle()) return;
		const worktree = pending.currentWorktree ?? pending.repository;
		const status = await pi.exec("git", ["diff", "--name-only", "--diff-filter=U"], {
			cwd: worktree,
			timeout: 10_000,
		});
		if (status.stdout.trim().length > 0) {
			await handleStackRebaseOutput(`status=conflict branch=${pending.branch} base=unknown worktree=${worktree}`, ctx);
			return;
		}
		const markerFiles: string[] = [];
		for (const file of pending.currentFiles ?? []) {
			try {
				const text = await readFile(join(worktree, file), "utf8");
				if (text.includes("<<<<<<<") || text.includes("=======") || text.includes(">>>>>>>")) markerFiles.push(file);
			} catch {}
		}
		if (markerFiles.length > 0) {
			pending.attempts += 1;
			if (pending.attempts > pending.maxAttempts) {
				await finishPendingStackRebase(ctx, `Shirei stack rebase stopped after ${pending.maxAttempts} conflict attempt(s)`, "warning");
				return;
			}
			const prompt = formatRebasePrompt(
				pending.currentBranch ?? pending.branch,
				pending.currentBase ?? "unknown",
				worktree,
				markerFiles.map((file) => `- ${file}: conflict markers still present`).join("\n"),
			);
			pi.sendUserMessage(prompt);
			return;
		}
		if (pending.currentFiles?.length) {
			const added = await pi.exec("git", ["add", "--", ...pending.currentFiles], { cwd: worktree, timeout: 10_000 });
			if (added.code !== 0) {
				await finishPendingStackRebase(ctx, added.stderr.trim() || "git add failed for resolved conflict files", "error");
				return;
			}
		}
		ctx.ui.setStatus("shirei-stack-rebase", `Continuing stack rebase for ${pending.branch}…`);
		const result = await pi.exec("shirei", ["stack", "rebase", "--continue"], {
			cwd: pending.repository,
			timeout: 180_000,
		});
		if (result.code !== 0 || result.killed) {
			await finishPendingStackRebase(ctx, result.stderr.trim() || `shirei stack rebase --continue failed with ${result.code}`, "error");
			return;
		}
		await handleStackRebaseOutput(`${result.stdout}\n${result.stderr}`, ctx);
	};

	pi.on("agent_settled", async (_event, ctx) => {
		await continuePendingStackRebase(ctx);
	});

	pi.registerTool({
		name: "shirei_stack_rebase_assist",
		label: "Shirei Stack Rebase Assist",
		// Each call opens a confirmation dialog; parallel calls would show two at once.
		executionMode: "sequential",
		description:
			"Cascade rebase a Shirei stack and use Pi with the fastest available GPT-5.6 Luna/Terra/Sol model to fix conflicts. Requires interactive confirmation.",
		promptSnippet: "Cascade rebase a Shirei stack with LLM conflict repair",
		promptGuidelines: [
			"Use shirei_stack_rebase_assist only when the user explicitly asks for an LLM-assisted cascade rebase.",
			"The tool may trigger model calls and source edits; keep conflict fixes focused and do not continue or abort the rebase manually.",
		],
		parameters: Type.Object({
			repository: Type.Optional(Type.String({ description: "Target repository path; defaults to the current repository" })),
			branch: Type.String({ description: "Leaf branch to cascade rebase through" }),
			onto: Type.Optional(Type.String({ description: "Override the root rebase target" })),
			modelOrder: Type.Optional(
				Type.Array(Type.String(), {
					description:
						"Optional speed-first model IDs; defaults to openai/gpt-5.6-luna, openai/gpt-5.6-terra, openai/gpt-5.6-sol",
				}),
			),
			maxConflictAttempts: Type.Optional(Type.Number({ description: "Retry limit per conflict stop; default 2" })),
		}),
		async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
			if (ctx.mode !== "tui") {
				throw new Error("shirei_stack_rebase_assist requires Pi interactive mode");
			}
			if (pendingStackRebase) {
				throw new Error("A Shirei stack rebase assist is already in progress");
			}
			const requestedRepository = repositoryInput(params.repository, ctx.cwd);
			const rootResult = await pi.exec("git", ["-C", requestedRepository, "rev-parse", "--show-toplevel"], {
				timeout: 10_000,
			});
			if (rootResult.code !== 0) {
				throw new Error(rootResult.stderr.trim() || `${requestedRepository} is not inside a Git repository`);
			}
			const repository = await realpath(rootResult.stdout.trim()).catch(() => resolve(rootResult.stdout.trim()));
			const branch = params.branch.trim();
			if (!branch) throw new Error("A leaf branch is required");
			const branchCheck = await pi.exec("git", ["check-ref-format", "--branch", branch], {
				cwd: repository,
				timeout: 10_000,
			});
			if (branchCheck.code !== 0) {
				throw new Error(branchCheck.stderr.trim() || `Invalid branch name: ${branch}`);
			}
			const args = ["stack", "rebase", "--branch", branch, "--dry-run"];
			if (params.onto?.trim()) args.push("--onto", params.onto.trim());
			const dryRun = await pi.exec("shirei", args, { cwd: repository, timeout: 60_000 });
			if (dryRun.code !== 0 || dryRun.killed) {
				throw new Error(dryRun.stderr.trim() || `shirei stack rebase --dry-run exited with ${dryRun.code}`);
			}
			const confirmed = await ctx.ui.confirm(
				"Run LLM-assisted Shirei stack rebase?",
				`${repository}\nbranch: ${branch}\n\nPlan:\n${dryRun.stdout.trim()}\n\nConflicts will be sent to Pi for repair.`,
			);
			if (!confirmed) {
				return { content: [{ type: "text" as const, text: "Shirei stack rebase assist cancelled." }], details: { cancelled: true } };
			}
			const previousModel = ctx.model;
			const selected = await selectFastestRebaseModel(ctx, params.modelOrder?.length ? params.modelOrder : DEFAULT_REBASE_MODEL_ORDER);
			if (!selected) {
				throw new Error("No configured/authenticated GPT-5.6 Luna, Terra, or Sol model is available");
			}
			pendingStackRebase = {
				repository,
				branch,
				attempts: 0,
				maxAttempts: Math.max(1, Math.floor(params.maxConflictAttempts ?? 2)),
				previousModel,
				modelLabel: selected.label,
			};
			ctx.ui.setStatus("shirei-stack-rebase", `Rebasing ${branch} with ${selected.label}…`);
			const runArgs = ["stack", "rebase", "--branch", branch];
			if (params.onto?.trim()) runArgs.push("--onto", params.onto.trim());
			const result = await pi.exec("shirei", runArgs, { cwd: repository, timeout: 180_000 });
			if (result.code !== 0 || result.killed) {
				await finishPendingStackRebase(ctx, result.stderr.trim() || `shirei stack rebase exited with ${result.code}`, "error");
				throw new Error(result.stderr.trim() || `shirei stack rebase exited with ${result.code}`);
			}
			await handleStackRebaseOutput(`${result.stdout}\n${result.stderr}`, ctx);
			return {
				content: [{ type: "text" as const, text: "Started Shirei stack rebase assist." }],
				details: { repository, branch, model: selected.label, pending: Boolean(pendingStackRebase) },
			};
		},
	});

	pi.registerTool({
		name: "shirei_pr_create",
		label: "Shirei PR Create",
		executionMode: "sequential",
		description:
			"Create or reuse a standalone GitHub pull request or GitLab merge request with an agent-authored title and description, preview, and confirmation.",
		promptSnippet: "Author, preview, and confirm a standalone pull request or merge request",
		promptGuidelines: [
			"Before using shirei_pr_create, inspect the branch diff and commits against its review base.",
			"Use shirei_pr_create instead of calling gh pr create or glab mr create directly for standalone reviews.",
			"Write a concise review title and a polished body that explains motivation, key decisions, system impact, testing, and stack position when applicable; omit empty sections and placeholders.",
			"Use Shirei stack submission instead when the branch belongs to a tracked Shirei stack.",
		],
		parameters: Type.Object({
			repository: Type.Optional(
				Type.String({ description: "Target repository path; defaults to the current repository" }),
			),
			base: Type.Optional(Type.String({ description: "Target branch; defaults to the remote's HEAD branch" })),
			title: Type.String({ description: "Concise agent-authored review title based on the branch diff" }),
			body: Type.String({
				description:
					"Polished agent-authored review description covering motivation, decisions, impact, testing, and stack position when relevant",
			}),
			draft: Type.Optional(Type.Boolean({ description: "Create the review as a draft" })),
		}),
		async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
			if (ctx.mode !== "tui") {
				throw new Error("shirei_pr_create requires Pi interactive mode so review creation can be confirmed");
			}

			const requestedRepository = repositoryInput(params.repository, ctx.cwd);
			const rootResult = await pi.exec("git", ["-C", requestedRepository, "rev-parse", "--show-toplevel"], {
				timeout: 10_000,
			});
			if (rootResult.code !== 0) {
				throw new Error(rootResult.stderr.trim() || `${requestedRepository} is not inside a Git repository`);
			}
			const repository = await realpath(rootResult.stdout.trim()).catch(() => resolve(rootResult.stdout.trim()));
			const title = params.title.trim();
			const body = params.body.trim();
			if (!title || !body) {
				throw new Error("shirei_pr_create requires a non-empty agent-authored title and body");
			}
			const args = ["pr", "create", "--title", title, "--body", body];
			if (params.base !== undefined) {
				args.push("--base", params.base);
			}
			if (params.draft === true) {
				args.push("--draft");
			}

			const preview = await pi.exec("shirei", [...args, "--dry-run"], {
				cwd: repository,
				timeout: 60_000,
			});
			if (preview.code !== 0 || preview.killed) {
				throw new Error(preview.stderr.trim() || `shirei pr create --dry-run exited with ${preview.code}`);
			}
			const previewText = preview.stdout.trim();
			const plan = new Map<string, string>();
			for (const line of previewText.split("\n")) {
				const separator = line.indexOf("=");
				if (separator > 0) {
					plan.set(line.slice(0, separator), line.slice(separator + 1));
				}
			}
			const branch = plan.get("branch");
			const head = plan.get("head");
			const base = plan.get("base");
			const remote = plan.get("remote");
			const remoteTarget = plan.get("remote_target");
			if (!branch || !head || !base || !remote || !remoteTarget) {
				throw new Error("shirei pr create --dry-run returned an incomplete review plan");
			}
			const confirmed = await ctx.ui.confirm(
				"Create pull request or merge request?",
				previewText,
			);
			if (!confirmed) {
				return {
					content: [{ type: "text" as const, text: "Shirei review creation cancelled." }],
					details: {
						repository,
						preview: previewText,
						cancelled: true,
					},
				};
			}

			const confirmedArgs = [
				...args,
				...(params.base === undefined ? ["--base", base] : []),
				"--remote",
				remote,
				"--expected-branch",
				branch,
				"--expected-head",
				head,
				"--expected-remote",
				remoteTarget,
			];
			const result = await pi.exec("shirei", confirmedArgs, {
				cwd: repository,
				timeout: 180_000,
			});
			if (result.code !== 0 || result.killed) {
				throw new Error(result.stderr.trim() || `shirei pr create exited with ${result.code}`);
			}
			return {
				content: [
					{
						type: "text" as const,
						text: result.stdout.trim() || "Shirei created the review.",
					},
				],
				details: {
					repository,
					preview: previewText,
					cancelled: false,
				},
			};
		},
	});

	pi.registerTool({
		name: "shirei_handoff",
		label: "Shirei Handoff",
		executionMode: "sequential",
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

			const requestedRepository = repositoryInput(params.repository, ctx.cwd);
			const rootResult = await pi.exec("git", ["-C", requestedRepository, "rev-parse", "--show-toplevel"], {
				timeout: 10_000,
			});
			if (rootResult.code !== 0) {
				throw new Error(rootResult.stderr.trim() || `${requestedRepository} is not inside a Git repository`);
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
				branchResult = await pi.exec("shirei", ["branch", branch, "--base", base, "--open"], {
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
				"stack list",
				"stack branch",
				"stack track",
				"stack rebase --dry-run",
				"stack submit --dry-run",
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
				commandArgs = ["branch", branch, "--base", base, "--open"];
			} else if (parts[0] === "stack") {
				if (parts[1] === "branch") {
					if (!process.env.TMUX && !process.env.HERDR_ENV) {
						ctx.ui.notify("/shirei stack branch requires Pi to be running inside tmux or Herdr", "warning");
						return;
					}
					if (parts.length !== 5 || !(parts[3] === "--base" || parts[3] === "-b")) {
						ctx.ui.notify("Usage: /shirei stack branch <name> --base <branch>", "warning");
						return;
					}
					commandArgs = ["stack", "branch", parts[2], "--base", parts[4], "--open"];
				} else if (parts[1] === "track") {
					if (parts.length !== 5 || !(parts[3] === "--base" || parts[3] === "-b")) {
						ctx.ui.notify("Usage: /shirei stack track <branch> --base <branch>", "warning");
						return;
					}
					commandArgs = ["stack", "track", parts[2], "--base", parts[4]];
				} else if (parts[1] === "untrack" && parts.length === 3) {
					commandArgs = ["stack", "untrack", parts[2]];
				} else if (parts[1] === "list" && parts.slice(2).every((part) => ["--all", "--porcelain"].includes(part))) {
					commandArgs = parts;
				} else if (parts[1] === "rebase") {
					if (!parts.includes("--dry-run")) {
						ctx.ui.notify("Use shirei_stack_rebase_assist for LLM-assisted stack rebases, or add --dry-run", "warning");
						return;
					}
					commandArgs = parts;
				} else if (parts[1] === "submit") {
					if (!parts.includes("--dry-run")) {
						ctx.ui.notify("/shirei stack submit is high-impact; run it in a terminal for now", "warning");
						return;
					}
					commandArgs = parts;
				} else {
					ctx.ui.notify("Usage: /shirei stack branch|track|untrack|list|rebase --dry-run|submit --dry-run", "warning");
					return;
				}
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
