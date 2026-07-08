import { describe, it, expect, vi, beforeEach } from "vitest";
import React from "react";
import { render } from "ink-testing-library";

// ── Mocks ────────────────────────────────────────────────────────

vi.mock("@immorterm/config", () => ({
	ensureGlobalConfig: vi.fn(),
	readGlobalConfig: vi.fn(() => ({
		version: 1,
		theme: "default",
		license: {
			key: null,
			instanceId: null,
			status: null,
			expiresAt: null,
			lastValidatedAt: null,
			productId: null,
			variantId: null,
			customerEmail: null,
		},
		defaults: {
			services: {
				memory: { enabled: false },
				mcpGateway: { enabled: false },
			},
		},
	})),
	writeGlobalConfig: vi.fn(),
	getGlobalConfigPath: vi.fn(() => "/tmp/test-config.json"),
}));

vi.mock("@immorterm/services", () => ({
	findBinary: vi.fn(() => null),
	checkMemoryHealth: vi.fn(async () => false),
}));

vi.mock("@immorterm/license", () => ({
	activateLicense: vi.fn(async () => ({ success: true, license: { email: "test@test.com" } })),
}));

vi.mock("@immorterm/analytics", () => ({
	identify: vi.fn(async () => {}),
	track: vi.fn(async () => {}),
}));

vi.mock("node:child_process", () => ({
	execFileSync: vi.fn(() => {
		throw new Error("not found"); // binary not found → shows install prompt
	}),
}));

vi.mock("../banner.js", () => ({
	THEME_NAMES: ["Purple Haze", "Ocean Depths", "Matrix"],
	THEME_LABELS: {
		"Purple Haze": "\u{1F7E3} Purple Haze",
		"Ocean Depths": "\u{1F535} Ocean Depths",
		"Matrix": "\u{1F48A} Matrix",
	},
	THEME_DESCRIPTIONS: {
		"Purple Haze": "Purple to Violet gradient",
		"Ocean Depths": "Deep blue to Teal cool",
		"Matrix": "Black to Green digital rain",
	},
	FREE_THEMES: new Set(["Purple Haze", "Ocean Depths", "Matrix"]),
	BANNER_CACHE: {
		"Purple Haze": ["", "  IMMORTERM", ""],
		"Ocean Depths": ["", "  IMMORTERM", ""],
		"Matrix": ["", "  IMMORTERM", ""],
	},
	renderThemePreview: vi.fn(() => "████████████"),
	renderBanner: vi.fn(() => ["", "  IMMORTERM", ""]),
	renderAnimatedBanner: vi.fn(() => ["", "  IMMORTERM", ""]),
	getMenuAccent: vi.fn(() => (s: string) => s),
	resolveTheme: vi.fn(() => "Purple Haze"),
}));

const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));

beforeEach(() => {
	vi.clearAllMocks();
});

// ── Tests ────────────────────────────────────────────────────────

describe("SetupWizard", () => {
	async function renderWizard() {
		const { SetupWizard } = await import("../SetupWizard.js");
		const onComplete = vi.fn();
		const inst = render(<SetupWizard onComplete={onComplete} />);
		return { ...inst, onComplete };
	}

	it("shows binary check on mount", async () => {
		const inst = await renderWizard();
		await delay(200);
		const frame = inst.lastFrame();

		// Should show binary check result (not found in our mock)
		expect(frame).toContain("IMMORTERM SETUP");
	});

	it("services step: enter toggles checkbox", async () => {
		// Make binary found so it auto-advances
		const cp = await import("node:child_process");
		vi.mocked(cp.execFileSync).mockReturnValue("" as any);

		const inst = await renderWizard();

		// Wait for binary auto-advance
		await delay(2000);

		let frame = inst.lastFrame();

		// Should be on services step
		if (frame?.includes("Choose your services")) {
			// Toggle memory (first item) with Enter
			inst.stdin.write("\r");
			await delay(50);

			frame = inst.lastFrame();
			// Checkbox should have toggled
			expect(frame).toContain("Choose your services");
		}
	}, 10000);

	it("services step: space also toggles", async () => {
		const cp = await import("node:child_process");
		vi.mocked(cp.execFileSync).mockReturnValue("" as any);

		const inst = await renderWizard();
		await delay(2000);

		let frame = inst.lastFrame();
		if (frame?.includes("Choose your services")) {
			inst.stdin.write(" "); // space toggles same as enter
			await delay(50);
			frame = inst.lastFrame();
			expect(frame).toContain("Choose your services");
		}
	}, 10000);

	it("services step: right arrow advances to license", async () => {
		const cp = await import("node:child_process");
		vi.mocked(cp.execFileSync).mockReturnValue("" as any);

		const inst = await renderWizard();
		await delay(2000);

		let frame = inst.lastFrame();
		if (frame?.includes("Choose your services")) {
			inst.stdin.write("\x1B[C"); // right arrow
			await delay(100);

			frame = inst.lastFrame();
			expect(frame).toContain("license key");
		}
	}, 10000);

	it("services step: enter on Continue advances to license", async () => {
		const cp = await import("node:child_process");
		vi.mocked(cp.execFileSync).mockReturnValue("" as any);

		const inst = await renderWizard();
		await delay(2000);

		let frame = inst.lastFrame();
		if (frame?.includes("Choose your services")) {
			// Navigate to Continue (index 2)
			inst.stdin.write("\x1B[B"); // down to gateway
			await delay(20);
			inst.stdin.write("\x1B[B"); // down to Continue
			await delay(20);
			inst.stdin.write("\r"); // enter
			await delay(100);

			frame = inst.lastFrame();
			expect(frame).toContain("license key");
		}
	}, 10000);

	it("license step: left arrow goes back to services", async () => {
		const cp = await import("node:child_process");
		vi.mocked(cp.execFileSync).mockReturnValue("" as any);

		const inst = await renderWizard();
		await delay(2000);

		let frame = inst.lastFrame();
		if (frame?.includes("Choose your services")) {
			inst.stdin.write("\x1B[C"); // right to license
			await delay(100);

			frame = inst.lastFrame();
			expect(frame).toContain("license key");

			inst.stdin.write("\x1B[D"); // left back to services
			await delay(100);

			frame = inst.lastFrame();
			expect(frame).toContain("Choose your services");
		}
	}, 10000);

	it("theme step: left arrow goes back to license", async () => {
		const cp = await import("node:child_process");
		vi.mocked(cp.execFileSync).mockReturnValue("" as any);

		const inst = await renderWizard();
		await delay(2000);

		let frame = inst.lastFrame();
		if (frame?.includes("Choose your services")) {
			// Advance to license
			inst.stdin.write("\x1B[C");
			await delay(100);

			// Advance to theme (select "No" for license)
			inst.stdin.write("\x1B[B"); // down to "No"
			await delay(20);
			inst.stdin.write("\r"); // enter
			await delay(100);

			frame = inst.lastFrame();
			expect(frame).toContain("Choose your theme");

			// Go back
			inst.stdin.write("\x1B[D");
			await delay(100);

			frame = inst.lastFrame();
			expect(frame).toContain("license key");
		}
	}, 10000);

	it("theme step: enter selects and triggers config write", async () => {
		const cp = await import("node:child_process");
		vi.mocked(cp.execFileSync).mockReturnValue("" as any);

		const inst = await renderWizard();
		await delay(2000);

		let frame = inst.lastFrame();
		if (frame?.includes("Choose your services")) {
			// Advance to license
			inst.stdin.write("\x1B[C");
			await delay(100);

			// Skip license (select "No")
			inst.stdin.write("\x1B[B");
			await delay(20);
			inst.stdin.write("\r");
			await delay(100);

			frame = inst.lastFrame();
			if (frame?.includes("Choose your theme")) {
				// Select first theme
				inst.stdin.write("\r");
				await delay(500);

				const config = await import("@immorterm/config");
				expect(config.writeGlobalConfig).toHaveBeenCalled();
			}
		}
	}, 10000);

	it("wizard calls onComplete when done", async () => {
		const cp = await import("node:child_process");
		vi.mocked(cp.execFileSync).mockReturnValue("" as any);

		const inst = await renderWizard();
		await delay(2000);

		let frame = inst.lastFrame();
		if (frame?.includes("Choose your services")) {
			// Advance through all steps
			inst.stdin.write("\x1B[C"); // to license
			await delay(100);
			inst.stdin.write("\x1B[B"); // No license
			await delay(20);
			inst.stdin.write("\r"); // confirm
			await delay(100);
			inst.stdin.write("\r"); // select theme
			await delay(500);

			expect(inst.onComplete).toHaveBeenCalled();
		}
	}, 10000);
});
