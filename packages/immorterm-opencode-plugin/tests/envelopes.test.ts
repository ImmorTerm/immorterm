import { describe, expect, it } from "vitest";
import {
	chatMessageToEnvelope,
	fileEditedToEnvelope,
	sessionCompactedToEnvelope,
	sessionCreatedToEnvelope,
	sessionDeletedToEnvelope,
	toolExecuteAfterToEnvelope,
	toolExecuteBeforeToEnvelope,
} from "../src/envelopes.js";

const ctx = { cwd: "/tmp/proj" };

describe("chatMessageToEnvelope", () => {
	it("maps a user message to UserPromptSubmit", () => {
		const env = chatMessageToEnvelope(
			{ sessionID: "s1" },
			{
				message: { role: "user" },
				parts: [
					{ type: "text", text: "hello" },
					{ type: "text", text: "world" },
					{ type: "image", url: "x" },
				],
			},
			ctx,
		);
		expect(env).not.toBeNull();
		expect(env?.hook_event_name).toBe("UserPromptSubmit");
		expect(env?.session_id).toBe("s1");
		expect(env?.cwd).toBe("/tmp/proj");
		expect(env?.prompt).toBe("hello\nworld");
		expect(env?.immorterm_vendor).toBe("opencode");
	});

	it("returns null for assistant messages", () => {
		const env = chatMessageToEnvelope(
			{ sessionID: "s1" },
			{ message: { role: "assistant" }, parts: [{ type: "text", text: "hi" }] },
			ctx,
		);
		expect(env).toBeNull();
	});
});

describe("toolExecuteBeforeToEnvelope", () => {
	it("maps to PreToolUse with tool name + args", () => {
		const env = toolExecuteBeforeToEnvelope(
			{ tool: "Bash", sessionID: "s2", callID: "c1" },
			{ args: { command: "ls" } },
			ctx,
		);
		expect(env.hook_event_name).toBe("PreToolUse");
		expect(env.tool_name).toBe("Bash");
		expect(env.tool_input).toEqual({ command: "ls" });
		expect(env.session_id).toBe("s2");
	});

	it("wraps non-object args under .value", () => {
		const env = toolExecuteBeforeToEnvelope(
			{ tool: "Read", sessionID: "s2", callID: "c1" },
			{ args: "raw-string" },
			ctx,
		);
		expect(env.tool_input).toEqual({ value: "raw-string" });
	});
});

describe("toolExecuteAfterToEnvelope", () => {
	it("maps to PostToolUse with tool_response", () => {
		const env = toolExecuteAfterToEnvelope(
			{ tool: "Edit", sessionID: "s3", callID: "c2", args: { file_path: "/x.ts" } },
			{ title: "Edit /x.ts", output: "ok", metadata: { ok: true } },
			ctx,
		);
		expect(env.hook_event_name).toBe("PostToolUse");
		expect(env.tool_name).toBe("Edit");
		expect(env.tool_input).toEqual({ file_path: "/x.ts" });
		expect(env.tool_response).toEqual({
			title: "Edit /x.ts",
			output: "ok",
			metadata: { ok: true },
		});
	});
});

describe("session lifecycle envelopes", () => {
	it("session.created → SessionStart", () => {
		const env = sessionCreatedToEnvelope({ id: "s4", directory: "/proj/a" }, ctx);
		expect(env.hook_event_name).toBe("SessionStart");
		expect(env.session_id).toBe("s4");
		expect(env.cwd).toBe("/proj/a");
	});

	it("falls back to ctx.cwd when info has no directory", () => {
		const env = sessionCreatedToEnvelope({ id: "s4" }, ctx);
		expect(env.cwd).toBe("/tmp/proj");
	});

	it("session.compacted → PreCompact", () => {
		const env = sessionCompactedToEnvelope("s5", ctx);
		expect(env.hook_event_name).toBe("PreCompact");
		expect(env.session_id).toBe("s5");
	});

	it("session.deleted → Stop", () => {
		const env = sessionDeletedToEnvelope({ id: "s6" }, ctx);
		expect(env.hook_event_name).toBe("Stop");
		expect(env.session_id).toBe("s6");
	});
});

describe("fileEditedToEnvelope", () => {
	it("maps to PostToolUse(Edit)", () => {
		const env = fileEditedToEnvelope({ file: "/proj/x.ts" }, "s7", ctx);
		expect(env.hook_event_name).toBe("PostToolUse");
		expect(env.tool_name).toBe("Edit");
		expect(env.tool_input).toEqual({ file_path: "/proj/x.ts" });
		expect(env.session_id).toBe("s7");
	});
});

describe("envelope shape invariants", () => {
	it("every envelope tags vendor=opencode and carries source_event", () => {
		const envelopes = [
			chatMessageToEnvelope(
				{ sessionID: "s" },
				{ message: { role: "user" }, parts: [{ type: "text", text: "a" }] },
				ctx,
			),
			toolExecuteBeforeToEnvelope({ tool: "T", sessionID: "s", callID: "c" }, { args: {} }, ctx),
			toolExecuteAfterToEnvelope(
				{ tool: "T", sessionID: "s", callID: "c", args: {} },
				{ title: "t", output: "o", metadata: null },
				ctx,
			),
			sessionCreatedToEnvelope({ id: "s" }, ctx),
			sessionCompactedToEnvelope("s", ctx),
			sessionDeletedToEnvelope({ id: "s" }, ctx),
			fileEditedToEnvelope({ file: "/x" }, "s", ctx),
		];
		for (const env of envelopes) {
			expect(env).not.toBeNull();
			expect(env?.immorterm_vendor).toBe("opencode");
			expect(typeof env?.source_event).toBe("string");
			expect(env?.session_id).toBeTruthy();
		}
	});
});
