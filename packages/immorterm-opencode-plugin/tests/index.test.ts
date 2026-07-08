import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ImmortermPlugin } from "../src/index.js";

let projectDir: string;

beforeEach(async () => {
	projectDir = await mkdtemp(resolve(tmpdir(), "immorterm-opencode-plugin-"));
});

afterEach(async () => {
	await rm(projectDir, { recursive: true, force: true });
});

async function readInbox(dir: string): Promise<unknown[]> {
	const inbox = resolve(dir, ".immorterm/hooks/inbox");
	const files = await readdir(inbox).catch(() => []);
	const items = await Promise.all(
		files.map(async (f) => JSON.parse(await readFile(resolve(inbox, f), "utf8"))),
	);
	return items;
}

describe("ImmortermPlugin factory", () => {
	it("returns the expected hook surface", async () => {
		const hooks = await ImmortermPlugin({ directory: projectDir });
		expect(typeof hooks.event).toBe("function");
		expect(typeof hooks["chat.message"]).toBe("function");
		expect(typeof hooks["tool.execute.before"]).toBe("function");
		expect(typeof hooks["tool.execute.after"]).toBe("function");
		expect(typeof hooks["experimental.session.compacting"]).toBe("function");
	});

	it("forwards a tool.execute.before to the inbox", async () => {
		const hooks = await ImmortermPlugin({ directory: projectDir });
		await hooks["tool.execute.before"]?.(
			{ tool: "Bash", sessionID: "s1", callID: "c1" },
			{ args: { command: "ls" } },
		);
		const items = (await readInbox(projectDir)) as Array<Record<string, unknown>>;
		expect(items.length).toBe(1);
		expect(items[0]?.hook_event_name).toBe("PreToolUse");
		expect(items[0]?.tool_name).toBe("Bash");
		expect(items[0]?.session_id).toBe("s1");
	});

	it("event hook routes session.created → SessionStart envelope", async () => {
		const hooks = await ImmortermPlugin({ directory: projectDir });
		await hooks.event?.({
			event: {
				type: "session.created",
				properties: { info: { id: "s1", directory: projectDir } },
			},
		});
		const items = (await readInbox(projectDir)) as Array<Record<string, unknown>>;
		expect(items.length).toBe(1);
		expect(items[0]?.hook_event_name).toBe("SessionStart");
	});

	it("file.edited uses last seen sessionID", async () => {
		const hooks = await ImmortermPlugin({ directory: projectDir });
		// First, fire a tool call to populate lastSessionID.
		await hooks["tool.execute.before"]?.(
			{ tool: "Bash", sessionID: "S-LATEST", callID: "c1" },
			{ args: {} },
		);
		// Now file.edited fires (no sessionID in payload).
		await hooks.event?.({
			event: { type: "file.edited", properties: { file: "/proj/x.ts" } },
		});
		const items = (await readInbox(projectDir)) as Array<Record<string, unknown>>;
		expect(items.length).toBe(2);
		const fileEnv = items.find((e) => e.tool_name === "Edit");
		expect(fileEnv?.session_id).toBe("S-LATEST");
	});

	it("file.edited is dropped when no session has been seen yet", async () => {
		const hooks = await ImmortermPlugin({ directory: projectDir });
		await hooks.event?.({
			event: { type: "file.edited", properties: { file: "/proj/x.ts" } },
		});
		const items = await readInbox(projectDir);
		expect(items.length).toBe(0);
	});

	it("ignores assistant chat messages (no Claude equivalent)", async () => {
		const hooks = await ImmortermPlugin({ directory: projectDir });
		await hooks["chat.message"]?.(
			{ sessionID: "s1" },
			{ message: { role: "assistant" }, parts: [{ type: "text", text: "hi" }] },
		);
		const items = await readInbox(projectDir);
		expect(items.length).toBe(0);
	});

	it("ignores noise events (lsp, todo, etc.)", async () => {
		const hooks = await ImmortermPlugin({ directory: projectDir });
		await hooks.event?.({ event: { type: "lsp.client.diagnostics", properties: {} } });
		await hooks.event?.({ event: { type: "todo.updated", properties: {} } });
		await hooks.event?.({ event: { type: "vcs.branch.updated", properties: {} } });
		const items = await readInbox(projectDir);
		expect(items.length).toBe(0);
	});
});
