import { describe, it, expect, vi, beforeEach } from "vitest";

// ── Mocks ────────────────────────────────────────────────────────

vi.mock("@immorterm/services", () => ({
	refreshMemoryState: vi.fn(),
	findBinary: vi.fn(),
	checkGatewayHealth: vi.fn(),
	checkHookHealth: vi.fn().mockResolvedValue({
		installed: true,
		hookCount: 5,
		recentActivity: true,
		errorCount: 0,
		errors: [],
		daemonRunning: true,
	}),
	preflightMemoryBinary: vi.fn(() => null),
	GATEWAY_PORT: 9100,
}));

vi.mock("@immorterm/config", () => ({
	readGlobalConfig: vi.fn(() => ({
		version: 1,
		theme: "default",
		license: { key: null, status: null, customerEmail: null },
		defaults: {
			services: {
				memory: { enabled: false },
				mcpGateway: { enabled: false },
				graph: { enabled: false },
			},
		},
	})),
	IMMORTERM_GLOBAL_DIR: "/tmp/.immorterm-test",
}));

vi.mock("node:fs", () => ({
	existsSync: vi.fn(() => true),
}));

vi.mock("node:child_process", () => ({
	execFileSync: vi.fn(),
}));

beforeEach(() => {
	vi.clearAllMocks();
});

// ── Helper ───────────────────────────────────────────────────────

async function setupMocks(overrides: {
	memApiHealthy?: boolean;
	memRunning?: boolean;
	memMcpHealthy?: boolean;
	binaryPath?: string | null;
	configExists?: boolean;
	binaryFound?: boolean;
	binaryVersion?: string;
	memPreflightError?: string | null;
} = {}) {
	const services = await import("@immorterm/services");
	const fs = await import("node:fs");
	const cp = await import("node:child_process");

	vi.mocked(services.findBinary).mockReturnValue(
		overrides.binaryPath !== undefined
			? overrides.binaryPath
			: "/usr/local/bin/immorterm-memory",
	);

	vi.mocked(services.preflightMemoryBinary).mockReturnValue(
		overrides.memPreflightError ?? null,
	);

	vi.mocked(services.refreshMemoryState).mockResolvedValue({
		running: overrides.memRunning ?? true,
		apiHealthy: overrides.memApiHealthy ?? true,
		mcpHealthy: overrides.memMcpHealthy ?? true,
	} as any);

	vi.mocked(services.checkGatewayHealth).mockResolvedValue({
		running: false,
		healthy: false,
		port: 9100,
	} as any);

	vi.mocked(fs.existsSync).mockReturnValue(overrides.configExists ?? true);

	if (overrides.binaryFound === false) {
		vi.mocked(cp.execFileSync).mockImplementation((cmd: string) => {
			if (cmd === "immorterm") throw new Error("not found");
			if (cmd === "du") return "42M\t/tmp/.immorterm-test" as any;
			return "" as any;
		});
	} else {
		vi.mocked(cp.execFileSync).mockImplementation((cmd: string) => {
			if (cmd === "immorterm") return (overrides.binaryVersion ?? "ImmorTerm 0.9.3 (built 2026-01-01)") as any;
			if (cmd === "du") return "42M\t/tmp/.immorterm-test" as any;
			return "" as any;
		});
	}
}

// ── Tests ────────────────────────────────────────────────────────

describe("runDoctorChecks", () => {
	it("returns all expected check names", async () => {
		await setupMocks();
		const { runDoctorChecks } = await import("../doctor.js");

		const checks = await runDoctorChecks();
		const names = checks.map((c) => c.name);

		expect(names).toContain("Config");
		expect(names).toContain("Memory Binary");
		expect(names).toContain("Memory Service");
		expect(names).toContain("MCP Gateway");
		expect(names).toContain("Disk Usage");
		expect(names).toContain("Hooks");
		expect(names).toContain("License");
	});

	it("reports Memory Service healthy as pass", async () => {
		await setupMocks({ memApiHealthy: true });
		const { runDoctorChecks } = await import("../doctor.js");

		const checks = await runDoctorChecks();
		const mem = checks.find((c) => c.name === "Memory Service")!;

		expect(mem.status).toBe("pass");
		expect(mem.detail).toContain("API healthy");
	});

	it("reports Memory Service running but unhealthy as warn", async () => {
		await setupMocks({ memRunning: true, memApiHealthy: false });
		const { runDoctorChecks } = await import("../doctor.js");

		const checks = await runDoctorChecks();
		const mem = checks.find((c) => c.name === "Memory Service")!;

		expect(mem.status).toBe("warn");
	});

	it("reports Memory Service not running as fail", async () => {
		await setupMocks({ memRunning: false, memApiHealthy: false });
		const { runDoctorChecks } = await import("../doctor.js");

		const checks = await runDoctorChecks();
		const mem = checks.find((c) => c.name === "Memory Service")!;

		expect(mem.status).toBe("fail");
	});

	it("reports Config missing as warn", async () => {
		await setupMocks({ configExists: false });
		const { runDoctorChecks } = await import("../doctor.js");

		const checks = await runDoctorChecks();
		const config = checks.find((c) => c.name === "Config")!;

		expect(config.status).toBe("warn");
		expect(config.detail).toContain("Not initialized");
	});

	it("fails Memory Binary when the binary cannot execute (loader error)", async () => {
		await setupMocks({
			memPreflightError: "version `GLIBC_2.39' not found (required by /d/immorterm-memory)",
		});
		const { runDoctorChecks } = await import("../doctor.js");

		const checks = await runDoctorChecks();
		const binary = checks.find((c) => c.name === "Memory Binary")!;

		expect(binary.status).toBe("fail");
		expect(binary.detail).toContain("GLIBC_2.39");
		expect(binary.hint).toContain("glibc");
	});

	it("sets exit code 1 when a check fails", async () => {
		await setupMocks({ memRunning: false, memApiHealthy: false });
		const { doctorCommand } = await import("../doctor.js");

		const prevExitCode = process.exitCode;
		try {
			await (doctorCommand as any).run({ args: {} });
			expect(process.exitCode).toBe(1);
		} finally {
			process.exitCode = prevExitCode;
		}
	});

	it("reports Free tier license", async () => {
		await setupMocks();
		const { runDoctorChecks } = await import("../doctor.js");

		const checks = await runDoctorChecks();
		const license = checks.find((c) => c.name === "License")!;

		expect(license.status).toBe("pass");
		expect(license.detail).toContain("Free tier");
	});

	it("reports Pro tier license", async () => {
		await setupMocks();
		const config = await import("@immorterm/config");
		vi.mocked(config.readGlobalConfig).mockReturnValue({
			version: 1,
			license: { key: "abc123", status: "active", customerEmail: "user@test.com" },
			defaults: {
				services: {
					memory: { enabled: false },
					mcpGateway: { enabled: false },
					graph: { enabled: false },
				},
			},
		} as any);
		const { runDoctorChecks } = await import("../doctor.js");

		const checks = await runDoctorChecks();
		const license = checks.find((c) => c.name === "License")!;

		expect(license.status).toBe("pass");
		expect(license.detail).toContain("Pro");
	});
});
