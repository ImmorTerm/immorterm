/**
 * init --yes hook installation — the CLI onboarding gap.
 *
 * Runs the REAL shared hook-installer core (libs/services/src/hook-installer.ts)
 * against a temp project dir with HOME redirected to a temp dir, and asserts
 * that `immorterm init --yes` leaves the project with:
 *  - .immorterm/hooks/immorterm-*.sh hook scripts
 *  - .claude/settings.local.json hook + MCP server entries
 */

import { describe, it, expect, vi, beforeAll, afterAll } from "vitest";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

// HOME must point at a temp dir BEFORE any module captures os.homedir()
// (@immorterm/config computes its global paths at import time). Safe to do
// in plain top-level code because every app module (and therefore every
// mock factory) is only loaded via `await import("../init.js")` inside tests.
const tmpHome = fs.mkdtempSync(path.join(os.tmpdir(), "immorterm-init-home-"));
const tmpProject = fs.mkdtempSync(path.join(os.tmpdir(), "immorterm-init-proj-"));
process.env.HOME = tmpHome;

// Keep the real hook installer; stub the pieces that touch the outside world.
vi.mock("@immorterm/services", async (importOriginal) => ({
	...(await importOriginal<typeof import("@immorterm/services")>()),
	autoInstallExtension: vi.fn(async () => ({ attempted: false })),
	findBinary: vi.fn(() => "/fake/immorterm-memory"),
}));

vi.mock("@immorterm/analytics", () => ({
	identify: vi.fn(async () => {}),
	track: vi.fn(async () => {}),
}));

vi.mock("@immorterm/license", () => ({
	activateLicense: vi.fn(async () => ({ success: false, error: "not used" })),
}));

vi.mock("consola", () => ({
	default: {
		info: vi.fn(),
		warn: vi.fn(),
		error: vi.fn(),
		success: vi.fn(),
		start: vi.fn(),
		box: vi.fn(),
		prompt: vi.fn(),
	},
}));

const originalCwd = process.cwd();

beforeAll(() => {
	process.chdir(tmpProject);
});

afterAll(() => {
	process.chdir(originalCwd);
	fs.rmSync(tmpHome, { recursive: true, force: true });
	fs.rmSync(tmpProject, { recursive: true, force: true });
});

describe("init --yes installs memory hooks into the current project", () => {
	it("creates .immorterm/hooks/immorterm-*.sh and registers them in .claude/settings.local.json", async () => {
		const { runInit } = await import("../init.js");
		await runInit({ yes: true }); // memory defaults to enabled

		// Hook scripts
		const hooksDir = path.join(tmpProject, ".immorterm", "hooks");
		const hookFiles = fs
			.readdirSync(hooksDir)
			.filter((f) => f.startsWith("immorterm-") && f.endsWith(".sh"));
		expect(hookFiles).toContain("immorterm-memory-guide.sh");
		expect(hookFiles).toContain("immorterm-session-end.sh");
		expect(hookFiles.length).toBeGreaterThanOrEqual(10);

		// Project identity minted
		const identity = JSON.parse(
			fs.readFileSync(path.join(tmpProject, ".immorterm", "project.json"), "utf-8"),
		);
		expect(identity.id).toMatch(/[0-9a-f-]{36}/);

		// settings.local.json entries
		const settings = JSON.parse(
			fs.readFileSync(path.join(tmpProject, ".claude", "settings.local.json"), "utf-8"),
		);
		const sessionStart = JSON.stringify(settings.hooks?.SessionStart ?? []);
		expect(sessionStart).toContain("immorterm-memory-guide.sh");
		expect(settings.hooks?.Stop).toBeDefined();
		expect(settings.mcpServers?.["immorterm-memory"]?.url).toContain(
			`/mcp/claude-code/${identity.id}`,
		);
	});

	it("is idempotent — re-running init does not duplicate hook entries", async () => {
		const { runInit } = await import("../init.js");
		await runInit({ yes: true });

		const settings = JSON.parse(
			fs.readFileSync(path.join(tmpProject, ".claude", "settings.local.json"), "utf-8"),
		);
		const guideEntries = JSON.stringify(settings.hooks.SessionStart).match(
			/immorterm-memory-guide\.sh/g,
		);
		expect(guideEntries).toHaveLength(1);
	});

	it("skips hook install when memory is disabled", async () => {
		const disabledProject = fs.mkdtempSync(path.join(os.tmpdir(), "immorterm-init-off-"));
		process.chdir(disabledProject);
		try {
			const { runInit } = await import("../init.js");
			await runInit({ yes: true, memory: false });
			expect(fs.existsSync(path.join(disabledProject, ".immorterm", "hooks"))).toBe(false);
		} finally {
			process.chdir(tmpProject);
			fs.rmSync(disabledProject, { recursive: true, force: true });
		}
	});
});
