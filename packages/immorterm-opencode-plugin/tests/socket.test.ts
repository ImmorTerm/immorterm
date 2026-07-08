import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { inboxDirFor, postToHub } from "../src/socket.js";

let projectDir: string;

beforeEach(async () => {
	projectDir = await mkdtemp(resolve(tmpdir(), "immorterm-opencode-test-"));
});

afterEach(async () => {
	await rm(projectDir, { recursive: true, force: true });
});

describe("postToHub", () => {
	it("writes a JSON file to <project>/.immorterm/hooks/inbox/", async () => {
		const env = {
			hook_event_name: "PreToolUse",
			session_id: "abc",
			cwd: projectDir,
			tool_name: "Bash",
			tool_input: { command: "echo hi" },
			immorterm_vendor: "opencode",
		};

		const written = await postToHub(env, { projectDir });

		expect(written.startsWith(inboxDirFor(projectDir))).toBe(true);
		expect(written.endsWith(".json")).toBe(true);

		const files = await readdir(inboxDirFor(projectDir));
		expect(files.length).toBe(1);
		expect(files[0]).toMatch(/^opencode-\d+-[0-9a-f]+\.json$/);

		const body = await readFile(written, "utf8");
		expect(JSON.parse(body)).toEqual(env);
	});

	it("creates the inbox directory if it does not exist", async () => {
		await postToHub({ x: 1 }, { projectDir });
		const files = await readdir(inboxDirFor(projectDir));
		expect(files.length).toBe(1);
	});

	it("produces unique filenames for back-to-back calls", async () => {
		const a = await postToHub({ k: 1 }, { projectDir });
		const b = await postToHub({ k: 2 }, { projectDir });
		const c = await postToHub({ k: 3 }, { projectDir });
		expect(new Set([a, b, c]).size).toBe(3);
	});

	it("honors an explicit inboxDir override", async () => {
		const overrideDir = resolve(projectDir, "custom-inbox");
		const written = await postToHub({ hello: "world" }, { projectDir, inboxDir: overrideDir });
		expect(written.startsWith(overrideDir)).toBe(true);
		const files = await readdir(overrideDir);
		expect(files.length).toBe(1);
	});

	it("does not leave .tmp files on success", async () => {
		await postToHub({ k: 1 }, { projectDir });
		const files = await readdir(inboxDirFor(projectDir));
		expect(files.some((f) => f.endsWith(".tmp"))).toBe(false);
	});
});
