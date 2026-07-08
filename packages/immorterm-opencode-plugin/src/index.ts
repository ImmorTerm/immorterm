/**
 * @immorterm/opencode-plugin — opencode plugin entry point.
 *
 * opencode is the only Phase A vendor that lacks stdin/stdout hooks.
 * Instead, it loads in-process TypeScript plugins via `opencode.json`:
 *
 *   { "plugin": ["@immorterm/opencode-plugin"] }
 *
 * This module exports the plugin factory expected by `@opencode-ai/plugin`.
 * Each fired event is mapped to a Claude-shape envelope (see ./envelopes)
 * and dropped into `<project>/.immorterm/hooks/inbox/` as a JSON file.
 * The existing hooks daemon (vendor-agnostic) then forwards each file to
 * the same hook script other vendors invoke directly.
 */

import {
	chatMessageToEnvelope,
	type ChatMessageInput,
	type ChatMessageOutput,
	type ClaudeShapeEnvelope,
	fileEditedToEnvelope,
	type FileEditedProps,
	sessionCompactedToEnvelope,
	sessionCreatedToEnvelope,
	sessionDeletedToEnvelope,
	type SessionInfo,
	toolExecuteAfterToEnvelope,
	type ToolExecuteAfterInput,
	type ToolExecuteAfterOutput,
	toolExecuteBeforeToEnvelope,
	type ToolExecuteBeforeInput,
	type ToolExecuteBeforeOutput,
} from "./envelopes.js";
import { postToHub } from "./socket.js";

/**
 * Loose-typed plugin input — we intentionally avoid a hard dependency on
 * `@opencode-ai/plugin` types so the package builds standalone in CI.
 * The runtime surface used here (`directory`, `worktree`) is stable.
 */
export interface ImmortermPluginInput {
	directory: string;
	worktree?: string;
	project?: { id?: string; worktree?: string };
	[key: string]: unknown;
}

/**
 * The Hooks shape we return — kept loose for the same reason.
 */
export interface ImmortermPluginHooks {
	event?: (input: { event: { type: string; properties?: unknown } }) => Promise<void>;
	"chat.message"?: (input: ChatMessageInput, output: ChatMessageOutput) => Promise<void>;
	"tool.execute.before"?: (
		input: ToolExecuteBeforeInput,
		output: ToolExecuteBeforeOutput,
	) => Promise<void>;
	"tool.execute.after"?: (
		input: ToolExecuteAfterInput,
		output: ToolExecuteAfterOutput,
	) => Promise<void>;
	"experimental.session.compacting"?: (
		input: { sessionID: string },
		output: { context: string[]; prompt?: string },
	) => Promise<void>;
}

interface PluginState {
	cwd: string;
	/** Last sessionID seen — used by file.edited which lacks one. */
	lastSessionID: string | null;
}

/**
 * Resolve the project directory from opencode's plugin input.
 * Prefers `directory` (the active workspace dir) and falls back to `worktree`.
 */
function resolveCwd(input: ImmortermPluginInput): string {
	if (typeof input.directory === "string" && input.directory.length > 0) {
		return input.directory;
	}
	if (typeof input.worktree === "string" && input.worktree.length > 0) {
		return input.worktree;
	}
	return process.cwd();
}

async function send(envelope: ClaudeShapeEnvelope, projectDir: string): Promise<void> {
	try {
		await postToHub(envelope as unknown as Record<string, unknown>, { projectDir });
	} catch (err) {
		// Never throw out of a plugin hook — opencode would surface it as
		// a session error. Log and move on.
		// eslint-disable-next-line no-console
		console.warn("[immorterm-opencode-plugin] failed to post envelope:", err);
	}
}

/**
 * Plugin factory matching `@opencode-ai/plugin`'s `Plugin` type.
 */
export const ImmortermPlugin = async (
	input: ImmortermPluginInput,
): Promise<ImmortermPluginHooks> => {
	const state: PluginState = {
		cwd: resolveCwd(input),
		lastSessionID: null,
	};

	return {
		// Catch-all event hook: opencode fires session.*, file.edited, etc.
		// here (they're not enumerated as named hooks in the SDK).
		event: async ({ event }) => {
			const props = (event.properties ?? {}) as Record<string, unknown>;
			switch (event.type) {
				case "session.created": {
					const info = props.info as SessionInfo | undefined;
					if (!info?.id) return;
					state.lastSessionID = info.id;
					await send(sessionCreatedToEnvelope(info, { cwd: state.cwd }), state.cwd);
					return;
				}
				case "session.updated": {
					const info = props.info as SessionInfo | undefined;
					if (info?.id) state.lastSessionID = info.id;
					// opencode fires session.updated frequently; we log-only by
					// dropping no envelope. (Spec: no Claude equivalent.)
					return;
				}
				case "session.compacted": {
					const sid = props.sessionID as string | undefined;
					if (!sid) return;
					await send(sessionCompactedToEnvelope(sid, { cwd: state.cwd }), state.cwd);
					return;
				}
				case "session.deleted": {
					const info = props.info as SessionInfo | undefined;
					if (!info?.id) return;
					await send(sessionDeletedToEnvelope(info, { cwd: state.cwd }), state.cwd);
					return;
				}
				case "file.edited": {
					const file = props.file as string | undefined;
					if (!file || !state.lastSessionID) return;
					await send(
						fileEditedToEnvelope({ file } as FileEditedProps, state.lastSessionID, {
							cwd: state.cwd,
						}),
						state.cwd,
					);
					return;
				}
				default:
					// Other events (lsp.*, message.part.*, todo.*, vcs.*) are
					// noise for digest. Drop them.
					return;
			}
		},

		"chat.message": async (msgInput, msgOutput) => {
			state.lastSessionID = msgInput.sessionID;
			const env = chatMessageToEnvelope(msgInput, msgOutput, { cwd: state.cwd });
			if (env) await send(env, state.cwd);
		},

		"tool.execute.before": async (toolInput, toolOutput) => {
			state.lastSessionID = toolInput.sessionID;
			await send(
				toolExecuteBeforeToEnvelope(toolInput, toolOutput, { cwd: state.cwd }),
				state.cwd,
			);
		},

		"tool.execute.after": async (toolInput, toolOutput) => {
			state.lastSessionID = toolInput.sessionID;
			await send(
				toolExecuteAfterToEnvelope(toolInput, toolOutput, { cwd: state.cwd }),
				state.cwd,
			);
		},

		// Pre-compact mirror — fires before opencode runs compaction.
		// Maps to PreCompact (matches Claude's hook semantics).
		"experimental.session.compacting": async (compactInput) => {
			await send(
				sessionCompactedToEnvelope(compactInput.sessionID, { cwd: state.cwd }),
				state.cwd,
			);
		},
	};
};

// Default export for `"plugin": ["@immorterm/opencode-plugin"]` resolution.
export default ImmortermPlugin;

// Re-exports for advanced consumers + tests.
export * from "./envelopes.js";
export { postToHub, inboxDirFor } from "./socket.js";
