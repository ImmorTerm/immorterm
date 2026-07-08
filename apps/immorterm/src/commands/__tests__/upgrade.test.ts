import { describe, it, expect, vi, beforeEach } from "vitest";

// ── Mocks ────────────────────────────────────────────────────────

vi.mock("@immorterm/services", () => ({
	getAllVersions: vi.fn(),
	checkCliUpdate: vi.fn(),
	getCliVersion: vi.fn(() => "0.1.0"),
	findBinary: vi.fn(() => "/tmp/.immorterm-test/bin/immorterm-memory"),
	installMemoryBinary: vi.fn(async () => "/tmp/.immorterm-test/bin/immorterm-memory"),
	stopMemory: vi.fn(async () => {}),
	startMemory: vi.fn(async () => ({ running: true, apiHealthy: true, mcpHealthy: true })),
	MEMORY_BINARY: "/tmp/.immorterm-test/bin/immorterm-memory",
}));

vi.mock("@immorterm/config", () => ({
	readGlobalConfig: vi.fn(),
	writeGlobalConfig: vi.fn(),
	getGlobalConfigPath: vi.fn(() => "/tmp/.immorterm-test/config.json"),
}));

vi.mock("node:fs", () => ({
	existsSync: vi.fn(() => true),
}));

beforeEach(async () => {
	vi.clearAllMocks();
	// Re-prime defaults (clearAllMocks keeps stale mockReturnValue overrides otherwise)
	const fs = await import("node:fs");
	const services = await import("@immorterm/services");
	vi.mocked(fs.existsSync).mockReturnValue(true);
	vi.mocked(services.findBinary).mockReturnValue("/tmp/.immorterm-test/bin/immorterm-memory");
	vi.mocked(services.installMemoryBinary).mockResolvedValue(
		"/tmp/.immorterm-test/bin/immorterm-memory",
	);
	vi.mocked(services.startMemory).mockResolvedValue({
		running: true,
		apiHealthy: true,
		mcpHealthy: true,
	} as any);
});

function mockAutoUpdateConfig(overrides: Partial<{
	enabled: boolean;
	checkIntervalHours: number;
	lastCheckedAt: string | null;
}> = {}) {
	return {
		version: 1,
		license: {},
		autoUpdate: {
			enabled: true,
			checkIntervalHours: 24,
			lastCheckedAt: null,
			...overrides,
		},
		defaults: { services: {} },
	} as any;
}

// ── upgradeMemory ────────────────────────────────────────────────

describe("upgradeMemory", () => {
	it("stops daemon, re-downloads binary, restarts", async () => {
		const services = await import("@immorterm/services");
		const { upgradeMemory } = await import("../upgrade.js");

		const ok = await upgradeMemory(false);

		expect(ok).toBe(true);
		expect(services.stopMemory).toHaveBeenCalledOnce();
		expect(services.installMemoryBinary).toHaveBeenCalledOnce();
		expect(services.startMemory).toHaveBeenCalledOnce();
		// Order: stop before install before start
		const stopOrder = vi.mocked(services.stopMemory).mock.invocationCallOrder[0]!;
		const installOrder = vi.mocked(services.installMemoryBinary).mock.invocationCallOrder[0]!;
		const startOrder = vi.mocked(services.startMemory).mock.invocationCallOrder[0]!;
		expect(stopOrder).toBeLessThan(installOrder);
		expect(installOrder).toBeLessThan(startOrder);
	});

	it("skips daemon stop when no binary is installed", async () => {
		const services = await import("@immorterm/services");
		vi.mocked(services.findBinary).mockReturnValue(null);
		const { upgradeMemory } = await import("../upgrade.js");

		const ok = await upgradeMemory(false);

		expect(ok).toBe(true);
		expect(services.stopMemory).not.toHaveBeenCalled();
		expect(services.installMemoryBinary).toHaveBeenCalledOnce();
	});

	it("returns false and restarts the old daemon when download fails", async () => {
		const services = await import("@immorterm/services");
		vi.mocked(services.installMemoryBinary).mockRejectedValue(new Error("HTTP 404"));
		const { upgradeMemory } = await import("../upgrade.js");

		const ok = await upgradeMemory(false);

		expect(ok).toBe(false);
		// Best-effort restart after failure
		expect(services.startMemory).toHaveBeenCalledOnce();
	});

	it("dry-run makes no changes", async () => {
		const services = await import("@immorterm/services");
		const { upgradeMemory } = await import("../upgrade.js");

		const ok = await upgradeMemory(true);

		expect(ok).toBe(true);
		expect(services.stopMemory).not.toHaveBeenCalled();
		expect(services.installMemoryBinary).not.toHaveBeenCalled();
		expect(services.startMemory).not.toHaveBeenCalled();
	});
});

// ── upgradeAi ────────────────────────────────────────────────────

describe("upgradeAi", () => {
	it("reports 'not yet distributable' without attempting npm/brew", async () => {
		const { upgradeAi } = await import("../upgrade.js");
		const ok = await upgradeAi(false);
		expect(ok).toBe(false);
		// No exec side effects to assert — the function is message-only by design
	});
});

// ── maybePrintUpgradeHint ────────────────────────────────────────

describe("maybePrintUpgradeHint", () => {
	it("does nothing when config file does not exist", async () => {
		const fs = await import("node:fs");
		const config = await import("@immorterm/config");
		const services = await import("@immorterm/services");
		vi.mocked(fs.existsSync).mockReturnValue(false);
		const { maybePrintUpgradeHint } = await import("../upgrade.js");

		await maybePrintUpgradeHint();

		expect(config.readGlobalConfig).not.toHaveBeenCalled();
		expect(services.checkCliUpdate).not.toHaveBeenCalled();
	});

	it("does nothing when auto-update is disabled", async () => {
		const config = await import("@immorterm/config");
		const services = await import("@immorterm/services");
		vi.mocked(config.readGlobalConfig).mockReturnValue(
			mockAutoUpdateConfig({ enabled: false }),
		);
		const { maybePrintUpgradeHint } = await import("../upgrade.js");

		await maybePrintUpgradeHint();

		expect(services.checkCliUpdate).not.toHaveBeenCalled();
		expect(config.writeGlobalConfig).not.toHaveBeenCalled();
	});

	it("skips the network check inside the interval", async () => {
		const config = await import("@immorterm/config");
		const services = await import("@immorterm/services");
		vi.mocked(config.readGlobalConfig).mockReturnValue(
			mockAutoUpdateConfig({ lastCheckedAt: new Date().toISOString() }),
		);
		const { maybePrintUpgradeHint } = await import("../upgrade.js");

		await maybePrintUpgradeHint();

		expect(services.checkCliUpdate).not.toHaveBeenCalled();
		expect(config.writeGlobalConfig).not.toHaveBeenCalled();
	});

	it("checks and stamps lastCheckedAt when stale", async () => {
		const config = await import("@immorterm/config");
		const services = await import("@immorterm/services");
		const stale = new Date(Date.now() - 25 * 3_600_000).toISOString();
		vi.mocked(config.readGlobalConfig).mockReturnValue(
			mockAutoUpdateConfig({ lastCheckedAt: stale }),
		);
		vi.mocked(services.checkCliUpdate).mockResolvedValue("0.2.0");
		const { maybePrintUpgradeHint } = await import("../upgrade.js");

		await maybePrintUpgradeHint();

		expect(services.checkCliUpdate).toHaveBeenCalledOnce();
		expect(config.writeGlobalConfig).toHaveBeenCalledOnce();
		const written = vi.mocked(config.writeGlobalConfig).mock.calls[0]![0] as any;
		expect(written.autoUpdate.lastCheckedAt).not.toBe(stale);
		expect(Date.parse(written.autoUpdate.lastCheckedAt)).toBeGreaterThan(Date.parse(stale));
	});

	it("stamps lastCheckedAt even when no update is available (no retry storm)", async () => {
		const config = await import("@immorterm/config");
		const services = await import("@immorterm/services");
		vi.mocked(config.readGlobalConfig).mockReturnValue(
			mockAutoUpdateConfig({ lastCheckedAt: null }),
		);
		vi.mocked(services.checkCliUpdate).mockResolvedValue(null);
		const { maybePrintUpgradeHint } = await import("../upgrade.js");

		await maybePrintUpgradeHint();

		expect(config.writeGlobalConfig).toHaveBeenCalledOnce();
	});
});
