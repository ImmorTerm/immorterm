/**
 * Maps opencode plugin SDK event payloads to Claude-shape hook envelopes
 * that ImmorTerm's vendor-agnostic hook scripts already understand.
 *
 * The Claude shape (canonical):
 *   {
 *     hook_event_name: "PreToolUse" | "PostToolUse" | "UserPromptSubmit"
 *                    | "SessionStart" | "PreCompact" | "Stop",
 *     session_id: string,
 *     cwd: string,
 *     tool_name?: string,
 *     tool_input?: Record<string, unknown>,
 *     tool_response?: Record<string, unknown>,
 *     prompt?: string,
 *     // ImmorTerm-side enrichment:
 *     immorterm_vendor: "opencode",
 *   }
 */

export type ClaudeHookEvent =
	| "PreToolUse"
	| "PostToolUse"
	| "UserPromptSubmit"
	| "SessionStart"
	| "PreCompact"
	| "Stop";

export interface ClaudeShapeEnvelope {
	hook_event_name: ClaudeHookEvent;
	session_id: string;
	cwd: string;
	tool_name?: string;
	tool_input?: Record<string, unknown>;
	tool_response?: Record<string, unknown>;
	prompt?: string;
	immorterm_vendor: "opencode";
	// Carried along for debugging/audit; hook scripts ignore unknown keys.
	source_event: string;
}

export interface EnvelopeContext {
	cwd: string;
}

// ---------- chat.message ----------

export interface ChatMessageInput {
	sessionID: string;
	agent?: string;
	model?: { providerID: string; modelID: string };
	messageID?: string;
	variant?: string;
}

export interface ChatMessagePart {
	type: string;
	text?: string;
	[key: string]: unknown;
}

export interface ChatMessageOutput {
	message: { role?: "user" | "assistant"; [key: string]: unknown };
	parts: ChatMessagePart[];
}

/**
 * Returns null for assistant messages (no Claude equivalent — log only).
 */
export function chatMessageToEnvelope(
	input: ChatMessageInput,
	output: ChatMessageOutput,
	ctx: EnvelopeContext,
): ClaudeShapeEnvelope | null {
	const role = output.message?.role;
	if (role !== "user") return null;

	const prompt = (output.parts ?? [])
		.filter((p) => p.type === "text" && typeof p.text === "string")
		.map((p) => p.text as string)
		.join("\n")
		.trim();

	return {
		hook_event_name: "UserPromptSubmit",
		session_id: input.sessionID,
		cwd: ctx.cwd,
		prompt,
		immorterm_vendor: "opencode",
		source_event: "chat.message",
	};
}

// ---------- tool.execute.before ----------

export interface ToolExecuteBeforeInput {
	tool: string;
	sessionID: string;
	callID: string;
}

export interface ToolExecuteBeforeOutput {
	args: unknown;
}

export function toolExecuteBeforeToEnvelope(
	input: ToolExecuteBeforeInput,
	output: ToolExecuteBeforeOutput,
	ctx: EnvelopeContext,
): ClaudeShapeEnvelope {
	return {
		hook_event_name: "PreToolUse",
		session_id: input.sessionID,
		cwd: ctx.cwd,
		tool_name: input.tool,
		tool_input: normalizeArgs(output.args),
		immorterm_vendor: "opencode",
		source_event: "tool.execute.before",
	};
}

// ---------- tool.execute.after ----------

export interface ToolExecuteAfterInput {
	tool: string;
	sessionID: string;
	callID: string;
	args: unknown;
}

export interface ToolExecuteAfterOutput {
	title: string;
	output: string;
	metadata: unknown;
}

export function toolExecuteAfterToEnvelope(
	input: ToolExecuteAfterInput,
	output: ToolExecuteAfterOutput,
	ctx: EnvelopeContext,
): ClaudeShapeEnvelope {
	return {
		hook_event_name: "PostToolUse",
		session_id: input.sessionID,
		cwd: ctx.cwd,
		tool_name: input.tool,
		tool_input: normalizeArgs(input.args),
		tool_response: {
			title: output.title,
			output: output.output,
			metadata: output.metadata,
		},
		immorterm_vendor: "opencode",
		source_event: "tool.execute.after",
	};
}

// ---------- session lifecycle (delivered via the `event` hook) ----------

export interface SessionInfo {
	id: string;
	directory?: string;
	[key: string]: unknown;
}

export function sessionCreatedToEnvelope(
	info: SessionInfo,
	ctx: EnvelopeContext,
): ClaudeShapeEnvelope {
	return {
		hook_event_name: "SessionStart",
		session_id: info.id,
		cwd: info.directory ?? ctx.cwd,
		immorterm_vendor: "opencode",
		source_event: "session.created",
	};
}

export function sessionCompactedToEnvelope(
	sessionID: string,
	ctx: EnvelopeContext,
): ClaudeShapeEnvelope {
	return {
		hook_event_name: "PreCompact",
		session_id: sessionID,
		cwd: ctx.cwd,
		immorterm_vendor: "opencode",
		source_event: "session.compacted",
	};
}

export function sessionDeletedToEnvelope(
	info: SessionInfo,
	ctx: EnvelopeContext,
): ClaudeShapeEnvelope {
	return {
		hook_event_name: "Stop",
		session_id: info.id,
		cwd: info.directory ?? ctx.cwd,
		immorterm_vendor: "opencode",
		source_event: "session.deleted",
	};
}

// ---------- file.edited ----------

export interface FileEditedProps {
	file: string;
}

/**
 * opencode's file.edited event has no sessionID; caller passes the
 * currently-active session captured from the most recent session.* event.
 */
export function fileEditedToEnvelope(
	props: FileEditedProps,
	sessionID: string,
	ctx: EnvelopeContext,
): ClaudeShapeEnvelope {
	return {
		hook_event_name: "PostToolUse",
		session_id: sessionID,
		cwd: ctx.cwd,
		tool_name: "Edit",
		tool_input: { file_path: props.file },
		immorterm_vendor: "opencode",
		source_event: "file.edited",
	};
}

// ---------- helpers ----------

function normalizeArgs(args: unknown): Record<string, unknown> {
	if (args && typeof args === "object" && !Array.isArray(args)) {
		return args as Record<string, unknown>;
	}
	return { value: args };
}
