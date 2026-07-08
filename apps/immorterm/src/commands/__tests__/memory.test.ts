import { describe, it, expect, vi, beforeEach } from "vitest";

// ── Mocks ────────────────────────────────────────────────────────

vi.mock("@immorterm/services", () => ({
	MEMORY_PID_FILE: "/tmp/.immorterm-test/memory.pid",
	MEMORY_LOG_FILE: "/tmp/.immorterm-test/memory-daemon.log",
	findBinary: vi.fn(),
	getMemoryPort: vi.fn(() => 8765),
	checkMemoryHealth: vi.fn(),
	installMemoryBinary: vi.fn(),
	preflightMemoryBinary: vi.fn(() => null),
	isMemoryFirstBoot: vi.fn(() => false),
	spawnMemoryDaemon: vi.fn(),
	tailMemoryLog: vi.fn(() => null),
	waitForMemory: vi.fn(),
}));

vi.mock("consola", () => ({
	default: {
		info: vi.fn(),
		warn: vi.fn(),
		error: vi.fn(),
		success: vi.fn(),
		start: vi.fn(),
	},
}));

beforeEach(async () => {
	vi.clearAllMocks();
	// clearAllMocks keeps mockReturnValue overrides — restore the defaults
	const services = await import("@immorterm/services");
	vi.mocked(services.getMemoryPort).mockReturnValue(8765);
	vi.mocked(services.preflightMemoryBinary).mockReturnValue(null);
	vi.mocked(services.isMemoryFirstBoot).mockReturnValue(false);
	vi.mocked(services.tailMemoryLog).mockReturnValue(null);
});

// ── Helpers ──────────────────────────────────────────────────────

async function runUp(): Promise<void> {
	const { memoryCommand } = await import("../memory.js");
	await (memoryCommand as any).run({ args: { action: "up" } });
}

function mockExit() {
	return vi.spyOn(process, "exit").mockImplementation(((code?: number) => {
		throw new Error(`process.exit(${code})`);
	}) as never);
}

function allOutput(consola: any): string {
	return [consola.info, consola.warn, consola.error, consola.success, consola.start]
		.flatMap((fn: any) => fn.mock.calls)
		.map((call: unknown[]) => call.join(" "))
		.join("\n");
}

// ── Tests ────────────────────────────────────────────────────────

describe("memory up", () => {
	it("prints the service URL when already healthy", async () => {
		const services = await import("@immorterm/services");
		vi.mocked(services.checkMemoryHealth).mockResolvedValue(true);
		const consola = (await import("consola")).default;

		await runUp();

		expect(allOutput(consola)).toContain("http://127.0.0.1:8765/health");
		expect(services.spawnMemoryDaemon).not.toHaveBeenCalled();
	});

	it("refuses to spawn on loader errors and explains glibc", async () => {
		const services = await import("@immorterm/services");
		vi.mocked(services.checkMemoryHealth).mockResolvedValue(false);
		vi.mocked(services.findBinary).mockReturnValue("/home/u/.immorterm/bin/immorterm-memory");
		vi.mocked(services.preflightMemoryBinary).mockReturnValue(
			"version `GLIBC_2.39' not found (required by immorterm-memory)",
		);
		const consola = (await import("consola")).default;
		const exit = mockExit();

		await expect(runUp()).rejects.toThrow("process.exit(1)");

		const output = allOutput(consola);
		expect(output).toContain("GLIBC_2.39");
		expect(output).toContain("glibc");
		expect(services.spawnMemoryDaemon).not.toHaveBeenCalled();
		exit.mockRestore();
	});

	it("starts with a 10s health wait on a warm boot and prints the URL", async () => {
		const services = await import("@immorterm/services");
		vi.mocked(services.checkMemoryHealth).mockResolvedValue(false);
		vi.mocked(services.findBinary).mockReturnValue("/home/u/.immorterm/bin/immorterm-memory");
		vi.mocked(services.isMemoryFirstBoot).mockReturnValue(false);
		vi.mocked(services.waitForMemory).mockResolvedValue(true);
		const consola = (await import("consola")).default;

		await runUp();

		expect(services.spawnMemoryDaemon).toHaveBeenCalledOnce();
		expect(vi.mocked(services.waitForMemory).mock.calls[0]![0]).toBe(10_000);
		expect(allOutput(consola)).toContain("http://127.0.0.1:8765/health");
	});

	it("warns about the model download and waits 180s on first boot", async () => {
		const services = await import("@immorterm/services");
		vi.mocked(services.checkMemoryHealth).mockResolvedValue(false);
		vi.mocked(services.findBinary).mockReturnValue("/home/u/.immorterm/bin/immorterm-memory");
		vi.mocked(services.isMemoryFirstBoot).mockReturnValue(true);
		vi.mocked(services.waitForMemory).mockResolvedValue(true);
		const consola = (await import("consola")).default;

		await runUp();

		expect(vi.mocked(services.waitForMemory).mock.calls[0]![0]).toBe(180_000);
		expect(allOutput(consola)).toContain("~150MB");
	});

	it("prints the daemon log path and tail on startup failure", async () => {
		const services = await import("@immorterm/services");
		vi.mocked(services.checkMemoryHealth).mockResolvedValue(false);
		vi.mocked(services.findBinary).mockReturnValue("/home/u/.immorterm/bin/immorterm-memory");
		vi.mocked(services.waitForMemory).mockResolvedValue(false);
		vi.mocked(services.tailMemoryLog).mockReturnValue("thread 'main' panicked at db open");
		const consola = (await import("consola")).default;
		const exit = mockExit();

		await expect(runUp()).rejects.toThrow("process.exit(1)");

		const output = allOutput(consola);
		expect(output).toContain("/tmp/.immorterm-test/memory-daemon.log");
		expect(output).toContain("panicked at db open");
		exit.mockRestore();
	});
});
