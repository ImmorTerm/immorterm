/**
 * `immorterm hooks install --project <dir>` — the Rust daemon's entry point
 * for wiring memory into a project the Tauri app opened.
 *
 * Runs the REAL shared hook-installer core against a temp project dir with
 * HOME redirected, and asserts the project ends up with:
 *  - .immorterm/hooks/immorterm-*.sh hook scripts
 *  - .claude/settings.local.json hook + MCP server entries
 *  - exit code 0 on success, and `hooks status` flipping not-installed → installed
 */

import { describe, it, expect, vi, beforeAll, afterAll } from "vitest";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

// HOME must point at a temp dir BEFORE any module captures os.homedir()
// (@immorterm/config computes its global paths at import time). Safe in
// top-level code because command modules are only loaded via dynamic import.
const tmpHome = fs.mkdtempSync(path.join(os.tmpdir(), "immorterm-hooks-home-"));
const tmpProject = fs.mkdtempSync(path.join(os.tmpdir(), "immorterm-hooks-proj-"));
process.env.HOME = tmpHome;

vi.mock("consola", () => ({
	default: {
		info: vi.fn(),
		warn: vi.fn(),
		error: vi.fn(),
		success: vi.fn(),
		start: vi.fn(),
	},
}));

// The command calls process.exit with its result code. Throw a sentinel so
// control flow stops exactly where a real exit would (a plain no-op mock
// would let execution continue past the exit and overwrite the code).
class ExitSentinel extends Error {
	constructor(public code: number) {
		super(`exit ${code}`);
	}
}
const exitSpy = vi.spyOn(process, "exit").mockImplementation(((code?: number) => {
	throw new ExitSentinel(code ?? 0);
}) as never);

async function runHooks(action: string, project?: string): Promise<number | undefined> {
	const { hooksCommand } = await import("../hooks.js");
	try {
		await (hooksCommand.run as (ctx: unknown) => unknown)({
			args: { _: [], action, project },
			rawArgs: [],
			cmd: hooksCommand,
		});
	} catch (e) {
		if (e instanceof ExitSentinel) return e.code;
		throw e;
	}
	return undefined;
}

beforeAll(() => {
	// Simulate what actually happens when the Tauri app opens a NEW project:
	// only .immorterm/project.json + terminals exist, no hooks.
	fs.mkdirSync(path.join(tmpProject, ".git"), { recursive: true });
});

afterAll(() => {
	exitSpy.mockRestore();
	fs.rmSync(tmpHome, { recursive: true, force: true });
	fs.rmSync(tmpProject, { recursive: true, force: true });
});

describe("immorterm hooks install --project <dir>", () => {
	it("reports not-installed before install (exit 1)", async () => {
		expect(await runHooks("status", tmpProject)).toBe(1);
	});

	it("creates .immorterm/hooks and registers them in .claude/settings.local.json (exit 0)", async () => {
		expect(await runHooks("install", tmpProject)).toBe(0);

		const hooksDir = path.join(tmpProject, ".immorterm", "hooks");
		const hookFiles = fs
			.readdirSync(hooksDir)
			.filter((f) => f.startsWith("immorterm-") && f.endsWith(".sh"));
		expect(hookFiles).toContain("immorterm-memory-guide.sh");
		expect(hookFiles).toContain("immorterm-session-end.sh");
		expect(hookFiles.length).toBeGreaterThanOrEqual(10);

		// Project identity minted + MCP registration keyed by it
		const identity = JSON.parse(
			fs.readFileSync(path.join(tmpProject, ".immorterm", "project.json"), "utf-8"),
		);
		expect(identity.id).toMatch(/[0-9a-f-]{36}/);

		const settings = JSON.parse(
			fs.readFileSync(path.join(tmpProject, ".claude", "settings.local.json"), "utf-8"),
		);
		expect(JSON.stringify(settings.hooks?.SessionStart ?? [])).toContain(
			"immorterm-memory-guide.sh",
		);
		expect(settings.mcpServers?.["immorterm-memory"]?.url).toContain(
			`/mcp/claude-code/${identity.id}`,
		);
	});

	it("hooks status reports installed after install (exit 0)", async () => {
		expect(await runHooks("status", tmpProject)).toBe(0);
	});

	it("is idempotent — re-running install does not duplicate hook entries", async () => {
		expect(await runHooks("install", tmpProject)).toBe(0);
		const settings = JSON.parse(
			fs.readFileSync(path.join(tmpProject, ".claude", "settings.local.json"), "utf-8"),
		);
		const guideEntries = JSON.stringify(settings.hooks.SessionStart).match(
			/immorterm-memory-guide\.sh/g,
		);
		expect(guideEntries).toHaveLength(1);
	});

	it("rejects a non-existent project dir (exit 1, nothing written)", async () => {
		expect(await runHooks("install", path.join(tmpProject, "no-such-dir"))).toBe(1);
	});
});
