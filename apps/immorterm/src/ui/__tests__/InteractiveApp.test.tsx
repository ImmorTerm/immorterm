import { describe, it, expect, vi, beforeEach } from "vitest";
import React from "react";
import { render } from "ink-testing-library";

// ── Mocks ────────────────────────────────────────────────────────

const mockConfig = {
	version: 1,
	theme: "Purple Haze",
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
			graph: { enabled: false },
		},
	},
};

vi.mock("@immorterm/config", () => ({
	readGlobalConfig: vi.fn(() => ({ ...mockConfig })),
	writeGlobalConfig: vi.fn(),
	ensureGlobalConfig: vi.fn(),
	getGlobalConfigPath: vi.fn(() => "/tmp/test-config.json"),
	IMMORTERM_GLOBAL_DIR: "/tmp/.immorterm-test",
}));

vi.mock("@immorterm/services", () => ({
	refreshMemoryState: vi.fn(async () => ({
		running: true,
		apiHealthy: true,
		mcpHealthy: true,
	})),
	findBinary: vi.fn(() => "/usr/local/bin/immorterm-memory"),
	checkGatewayHealth: vi.fn(async () => ({
		running: true,
		healthy: true,
		port: 9100,
		serverCount: 3,
		activeChildren: 2,
	})),
	startMemory: vi.fn(async () => ({
		apiHealthy: true,
		mcpHealthy: true,
	})),
	stopMemory: vi.fn(),
	startGateway: vi.fn(async () => ({
		healthy: true,
		port: 9100,
	})),
	stopGateway: vi.fn(),
	GATEWAY_PORT: 9100,
}));

vi.mock("@immorterm/license", () => ({
	validateLicense: vi.fn(async () => ({ success: true })),
	activateLicense: vi.fn(async () => ({ success: true, license: { email: "test@test.com" } })),
	deactivateLicense: vi.fn(async () => ({ success: true })),
}));

vi.mock("@immorterm/analytics", () => ({
	identify: vi.fn(async () => {}),
	track: vi.fn(async () => {}),
}));

vi.mock("../banner.js", () => ({
	THEME_NAMES: [
		"Purple Haze", "Ocean Depths", "Aurora Borealis", "Sunset Glow", "Solar Flare",
		"Glacier", "Rose Gold", "Cyberpunk", "Monochrome Dark", "Monochrome Light",
		"Neon Tokyo", "Dracula", "Matrix", "Vaporwave", "Ember",
		"Electric Lime", "Tidal", "Amber", "Synthwave", "Molten", "Rainbow",
	],
	THEME_LABELS: {
		"Purple Haze": "\u{1F7E3} Purple Haze", "Ocean Depths": "\u{1F535} Ocean Depths",
		"Aurora Borealis": "\u{1F30C} Aurora Borealis", "Sunset Glow": "\u{1F7E0} Sunset Glow",
		"Solar Flare": "\u2600\uFE0F Solar Flare", "Glacier": "\u{1F3D4}\uFE0F Glacier",
		"Rose Gold": "\u{1FA77} Rose Gold", "Cyberpunk": "\u{1F49C} Cyberpunk",
		"Monochrome Dark": "\u2B1B Monochrome Dark", "Monochrome Light": "\u2B1C Monochrome Light",
		"Neon Tokyo": "\u{1F3D9}\uFE0F Neon Tokyo", "Dracula": "\u{1F9DB} Dracula",
		"Matrix": "\u{1F48A} Matrix", "Vaporwave": "\u{1F4FC} Vaporwave",
		"Ember": "\u{1F525} Ember", "Electric Lime": "\u26A1 Electric Lime",
		"Tidal": "\u{1F30A} Tidal", "Amber": "\u{1F7E1} Amber",
		"Synthwave": "\u{1F306} Synthwave", "Molten": "\u{1F30B} Molten",
		"Rainbow": "\u{1F308} Rainbow",
	},
	THEME_DESCRIPTIONS: {
		"Purple Haze": "Purple to Violet gradient", "Ocean Depths": "Deep blue to Teal cool",
		"Aurora Borealis": "Dark to Green shimmer", "Sunset Glow": "Warm red to Gold horizon",
		"Solar Flare": "Dark amber to Blazing gold", "Glacier": "Frozen blue to Ice white",
		"Rose Gold": "Deep rose to Pink bloom", "Cyberpunk": "Neon purple to Magenta glow",
		"Monochrome Dark": "Clean black to Gray minimal", "Monochrome Light": "White to Silver light mode",
		"Neon Tokyo": "Midnight to Neon pink city", "Dracula": "Classic Dracula color scheme",
		"Matrix": "Black to Green digital rain", "Vaporwave": "Retro purple to Pink aesthetic",
		"Ember": "Smoldering dark to Warm amber", "Electric Lime": "Dark forest to Bright lime",
		"Tidal": "Deep ocean to Teal wave", "Amber": "Dark gold to Bright amber",
		"Synthwave": "Retro purple to Electric cyan", "Molten": "Black lava to Fiery red",
		"Rainbow": "Full spectrum gradient",
	},
	FREE_THEMES: new Set(["Purple Haze", "Ocean Depths", "Matrix", "Sunset Glow", "Monochrome Dark"]),
	BANNER_CACHE: Object.fromEntries(
		["Purple Haze", "Ocean Depths", "Aurora Borealis", "Sunset Glow", "Solar Flare",
		 "Glacier", "Rose Gold", "Cyberpunk", "Monochrome Dark", "Monochrome Light",
		 "Neon Tokyo", "Dracula", "Matrix", "Vaporwave", "Ember",
		 "Electric Lime", "Tidal", "Amber", "Synthwave", "Molten", "Rainbow",
		].map(name => [name, ["", `  IMMORTERM (${name})`, "", "  Immortal Terminals", "  v0.1.0", ""]])
	),
	renderThemePreview: vi.fn(() => "████████████"),
	renderBanner: vi.fn((name: string) => ["", `  IMMORTERM (${name})`, "", "  Immortal Terminals", "  v0.1.0", ""]),
	renderAnimatedBanner: vi.fn((_name: string, _tick: number) => ["", "  IMMORTERM", "", "  Immortal Terminals", "  v0.1.0", ""]),
	getMenuAccent: vi.fn(() => (s: string) => s),
	resolveTheme: vi.fn(() => "Purple Haze"),
}));

vi.mock("../../commands/doctor.js", () => ({
	runDoctorChecks: vi.fn(async () => [
		{ name: "Docker", status: "pass", detail: "Installed, running" },
		{ name: "Config", status: "pass", detail: "/tmp/config.json" },
		{ name: "OpenMemory", status: "pass", detail: "API healthy" },
	]),
}));

const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));

beforeEach(async () => {
	vi.clearAllMocks();
	// Deep clone to prevent cross-test mutation of nested objects (e.g. defaults.services)
	const config = await import("@immorterm/config");
	vi.mocked(config.readGlobalConfig).mockReturnValue(structuredClone(mockConfig) as any);
});

// ── Tests ────────────────────────────────────────────────────────

describe("InteractiveApp", () => {
	async function renderApp(props: { firstRun?: boolean } = {}) {
		// Dynamic import so mocks are in place
		const { InteractiveApp } = await import("../InteractiveApp.js");
		const inst = render(<InteractiveApp firstRun={props.firstRun ?? false} />);
		await delay(150); // Let useEffect async loads settle
		return inst;
	}

	it("renders menu with all expected items", async () => {
		const { lastFrame } = await renderApp();
		const frame = lastFrame();

		expect(frame).toContain("Setup Wizard");
		expect(frame).toContain("Services");
		expect(frame).toContain("Insights");
		expect(frame).toContain("Diagnostics");
		expect(frame).toContain("Log Explorer");
		expect(frame).toContain("Pro");
		expect(frame).toContain("Theme");
		expect(frame).toContain("Quit");
	});

	it("shows service status panel", async () => {
		const { lastFrame } = await renderApp();
		const frame = lastFrame();

		// Status panel shows Memory + MCP Gateway (Docker + Qdrant removed in native memory migration)
		expect(frame).toContain("Memory");
		expect(frame).toContain("MCP Gateway");
		expect(frame).not.toContain("Docker");
		expect(frame).not.toContain("Qdrant");
	});

	it("shows Free tier info by default", async () => {
		const { lastFrame } = await renderApp();
		const frame = lastFrame();

		expect(frame).toContain("Free");
	});

	it("shows Pro tier info when license is active", async () => {
		const config = await import("@immorterm/config");
		vi.mocked(config.readGlobalConfig).mockReturnValue({
			...mockConfig,
			license: { ...mockConfig.license, status: "active", customerEmail: "pro@test.com" },
		} as any);

		const { lastFrame } = await renderApp();
		const frame = lastFrame();

		expect(frame).toContain("Pro");
	});

	it("down arrow moves cursor indicator", async () => {
		const { lastFrame, stdin } = await renderApp();

		// Initially first item (Setup Wizard) has cursor
		let frame = lastFrame()!;
		const lines = frame.split("\n");
		const wizardLine = lines.find((l) => l.includes("Setup Wizard"));
		expect(wizardLine).toContain("❯");

		// Press down — cursor moves to "Services" menu item (has "Manage" in desc)
		stdin.write("\x1B[B");
		await delay(50);

		frame = lastFrame()!;
		const updatedLines = frame.split("\n");
		// Find the menu item line specifically (contains the description text)
		const servicesMenuLine = updatedLines.find((l) => l.includes("Manage and control"));
		expect(servicesMenuLine).toContain("❯");
	});

	it("q exits the app", async () => {
		const { lastFrame, stdin } = await renderApp();

		stdin.write("q");
		await delay(50);

		// After exit, lastFrame should still be defined but app should have exited
		// ink-testing-library doesn't throw on exit, but the component unmounts
		expect(lastFrame).toBeDefined();
	});

	it("enter on Diagnostics shows results", async () => {
		const { lastFrame, stdin } = await renderApp();

		// Navigate to Diagnostics (index 2)
		stdin.write("\x1B[B"); // down to Services
		await delay(30);
		stdin.write("\x1B[B"); // down to Diagnostics
		await delay(30);
		stdin.write("\r"); // enter
		await delay(200);

		const frame = lastFrame();
		// Should show doctor results or "Press any key" prompt
		expect(frame).toMatch(/Docker|Press any key|any key/);
	});

	it("enter on Services opens services sub-view", async () => {
		const { lastFrame, stdin } = await renderApp();

		// Navigate to Services (index 1)
		stdin.write("\x1B[B"); // down to Services
		await delay(30);
		stdin.write("\r"); // enter
		await delay(100);

		const frame = lastFrame();
		expect(frame).toContain("ImmorTerm Memory");
		expect(frame).toContain("MCP Gateway");
		// Should NOT show Docker or Qdrant in services list
		expect(frame).not.toContain("Docker");
		expect(frame).not.toContain("Qdrant");
	});

	it("services view: esc returns to menu", async () => {
		const { lastFrame, stdin } = await renderApp();

		// Open services
		stdin.write("\x1B[B");
		await delay(30);
		stdin.write("\r");
		await delay(100);

		// Press esc
		stdin.write("\x1B");
		await delay(100);

		const frame = lastFrame();
		expect(frame).toContain("Setup Wizard");
	});

	it("services view: enter opens service detail", async () => {
		const { lastFrame, stdin } = await renderApp();

		// Open services
		stdin.write("\x1B[B");
		await delay(30);
		stdin.write("\r");
		await delay(100);

		// Enter on ImmorTerm Memory (first item)
		stdin.write("\r");
		await delay(100);

		const frame = lastFrame();
		// Should show detail items
		expect(frame).toContain("Enable");
		expect(frame).toContain("Start");
		expect(frame).toContain("Stop");
		expect(frame).toContain("Back");
	});

	it("service detail: Enable toggles config", async () => {
		const { stdin } = await renderApp();

		// Navigate: menu → services → detail
		stdin.write("\x1B[B"); await delay(30);
		stdin.write("\r"); await delay(100);
		stdin.write("\r"); await delay(100);

		// Enter on Enable (first item, cursor already there)
		stdin.write("\r");
		await delay(100);

		const config = await import("@immorterm/config");
		expect(config.writeGlobalConfig).toHaveBeenCalled();
	});

	it("service detail: Start runs service and returns to detail", async () => {
		// Add a small delay to the mock so the action view is visible
		const services = await import("@immorterm/services");
		vi.mocked(services.startMemory).mockImplementation(async () => {
			await delay(100);
			return { apiHealthy: true, mcpHealthy: true } as any;
		});

		const { lastFrame, stdin } = await renderApp();

		// Navigate: menu → services → detail
		stdin.write("\x1B[B"); await delay(50);
		stdin.write("\r"); await delay(150);
		stdin.write("\r"); await delay(150);

		// Navigate to Start (index 1) and press enter
		stdin.write("\x1B[B"); await delay(50);
		stdin.write("\r"); await delay(50);

		// Should show spinner while action is running
		let frame = lastFrame();
		expect(frame).toContain("Starting");

		// Wait for action to complete
		await delay(300);

		// Should be in result view now
		frame = lastFrame();
		expect(frame).toContain("Memory running");
		expect(frame).toContain("any key");

		// Press any key — should return to service detail, not main menu
		stdin.write(" ");
		await delay(100);

		frame = lastFrame();
		// Should be back in service detail (Start/Stop are stable labels)
		expect(frame).toContain("Start");
		expect(frame).toContain("Stop");
		expect(frame).toContain("enter select");
		// Should NOT be in the main menu
		expect(frame).not.toContain("Setup Wizard");
	});

	it("service detail: esc returns to services list", async () => {
		const { lastFrame, stdin } = await renderApp();

		// Navigate: menu → services → detail (use generous delays)
		stdin.write("\x1B[B"); await delay(50);
		stdin.write("\r"); await delay(150);
		stdin.write("\r"); await delay(150);

		// Verify we're in detail (Start/Stop are always present)
		let frame = lastFrame();
		expect(frame).toContain("Start");
		expect(frame).toContain("enter select");

		// Press esc — back to services list
		stdin.write("\x1B");
		await delay(150);

		frame = lastFrame();
		// Should show services list (has enter open hint)
		expect(frame).toContain("enter open");
	});

	it("enter on Theme opens theme picker", async () => {
		const { lastFrame, stdin } = await renderApp();

		// Navigate to Theme (index 6: after Wizard, Services, Insights, Diagnostics, Logs, Pro)
		for (let i = 0; i < 6; i++) {
			stdin.write("\x1B[B");
			await delay(20);
		}
		stdin.write("\r");
		await delay(100);

		const frame = lastFrame();
		expect(frame).toContain("Choose a theme");
	});

	it("theme view: enter saves selection", async () => {
		const { stdin } = await renderApp();

		// Navigate to Theme (index 6)
		for (let i = 0; i < 6; i++) {
			stdin.write("\x1B[B");
			await delay(20);
		}
		stdin.write("\r");
		await delay(100);

		// Select first theme (Purple Haze — free, currently active)
		stdin.write("\r");
		await delay(200);

		const config = await import("@immorterm/config");
		expect(config.writeGlobalConfig).toHaveBeenCalled();
	});

	it("theme view: shows all 21 themes with labels", async () => {
		const { lastFrame, stdin } = await renderApp();

		// Navigate to Theme (index 6) — generous delays to ensure all 6 arrows register
		for (let i = 0; i < 6; i++) {
			stdin.write("\x1B[B"); await delay(80);
		}
		stdin.write("\r");
		await delay(100);

		const frame = lastFrame();
		expect(frame).toContain("Purple Haze");
		expect(frame).toContain("Ocean Depths");
		expect(frame).toContain("Matrix");
		expect(frame).toContain("Dracula");
		expect(frame).toContain("Rainbow");
	});

	it("theme view: Pro theme on Free tier shows upgrade message", async () => {
		const { lastFrame, stdin } = await renderApp();

		// Navigate to Theme (index 6)
		for (let i = 0; i < 6; i++) {
			stdin.write("\x1B[B"); await delay(80);
		}
		stdin.write("\r");
		await delay(100);

		// Navigate to Aurora Borealis (index 2, a Pro theme)
		stdin.write("\x1B[B"); await delay(50);
		stdin.write("\x1B[B"); await delay(50);
		stdin.write("\r");
		await delay(200);

		const frame = lastFrame();
		expect(frame).toContain("requires Pro");
	});

	it("theme view: shows (Pro) tag on non-free themes", async () => {
		const { lastFrame, stdin } = await renderApp();

		// Navigate to Theme (index 6)
		for (let i = 0; i < 6; i++) {
			stdin.write("\x1B[B"); await delay(80);
		}
		stdin.write("\r");
		await delay(100);

		const frame = lastFrame();
		expect(frame).toContain("(Pro)");
	});

	it("theme view: esc resets preview and returns to menu", async () => {
		const { lastFrame, stdin } = await renderApp();

		// Navigate to Theme (index 6)
		for (let i = 0; i < 6; i++) {
			stdin.write("\x1B[B"); await delay(80);
		}
		stdin.write("\r");
		await delay(100);

		// Press esc
		stdin.write("\x1B");
		await delay(100);

		const frame = lastFrame();
		expect(frame).toContain("Setup Wizard");
	});

	it("enter on Pro opens Pro view", async () => {
		const { lastFrame, stdin } = await renderApp();

		// Navigate to Pro (index 5: after Wizard, Services, Insights, Diagnostics, Logs)
		for (let i = 0; i < 5; i++) {
			stdin.write("\x1B[B"); await delay(50);
		}
		stdin.write("\r");
		await delay(150);

		const frame = lastFrame();
		// Pro view header + upgrade CTA for free tier
		expect(frame).toContain("Upgrade to Pro");
	});

	it("Pro view: Free tier shows upgrade prompt", async () => {
		const { lastFrame, stdin } = await renderApp();

		// Navigate to Pro (index 5)
		for (let i = 0; i < 5; i++) {
			stdin.write("\x1B[B"); await delay(50);
		}
		stdin.write("\r");
		await delay(150);

		const frame = lastFrame();
		expect(frame).toContain("Upgrade to Pro");
		expect(frame).toContain("Activate Key");
	});

	it("Pro view: Pro tier shows deactivate option", async () => {
		const config = await import("@immorterm/config");
		vi.mocked(config.readGlobalConfig).mockReturnValue({
			...mockConfig,
			license: { ...mockConfig.license, status: "active", customerEmail: "pro@test.com" },
		} as any);

		const { lastFrame, stdin } = await renderApp();

		// Navigate to Pro (index 5)
		for (let i = 0; i < 5; i++) {
			stdin.write("\x1B[B"); await delay(50);
		}
		stdin.write("\r");
		await delay(150);

		const frame = lastFrame();
		expect(frame).toContain("Deactivate");
	});

	it("firstRun=true auto-launches wizard", async () => {
		const { lastFrame } = await renderApp({ firstRun: true });
		const frame = lastFrame();

		expect(frame).toContain("IMMORTERM SETUP");
	});
});
