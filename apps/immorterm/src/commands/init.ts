/**
 * immorterm init — Setup wizard
 *
 * Interactive terminal → ink SetupWizard (rich TUI)
 * Interactive but ink fails → consola prompts (fallback)
 * Non-TTY / piped / --yes → no prompts, write defaults + flags directly
 *
 * Walks through:
 * 1. Theme selection
 * 2. Service selection (memory, gateway)
 * 3. AI tool (vendor) selection — per-project, pre-ticked from what's installed
 * 4. License key entry (optional)
 * 5. Write global config
 */

import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import {
	ensureGlobalConfig,
	readGlobalConfig,
	writeGlobalConfig,
} from "@immorterm/config";
import type { VendorId } from "@immorterm/config";
import {
	autoInstallExtension,
	detectVendors,
	detectedVendorIds,
	findBinary,
	installMemoryBinary,
	setProjectVendors,
} from "@immorterm/services";
import { activateLicense } from "@immorterm/license";
import { identify, track } from "@immorterm/analytics";
import { ensureProjectMemoryHooks } from "../lib/project-hooks.js";

const DEFAULT_THEME = "Purple Haze";

interface InitFlags {
	yes?: boolean;
	memory?: boolean;
	gateway?: boolean;
	theme?: string;
	licenseKey?: string;
	/** Comma-separated vendor ids. Omitted = whatever is detected on this machine. */
	vendors?: string;
}

/**
 * Vendor selection for a non-interactive run: the explicit flag if given,
 * otherwise whatever is installed AND has been used. A fresh machine with no
 * AI CLI at all still gets Claude Code, matching the shipped default.
 */
function resolveVendorFlag(flag: string | undefined): VendorId[] {
	if (flag !== undefined) {
		const known = new Set(detectVendors().map((p) => p.id as string));
		const asked = flag.split(",").map((s) => s.trim()).filter(Boolean);
		const unknown = asked.filter((v) => !known.has(v));
		if (unknown.length > 0) {
			consola.warn(`Unknown vendor id(s) ignored: ${unknown.join(", ")}`);
		}
		return asked.filter((v) => known.has(v)) as VendorId[];
	}
	const detected = detectedVendorIds();
	return detected.length > 0 ? detected : ["claudeCode"];
}

/** Run the ink-based rich TUI wizard. Resolves with the vendor selection. */
async function runInkWizard(): Promise<VendorId[] | null> {
	const React = await import("react");
	const { render } = await import("ink");
	const { SetupWizard } = await import("../ui/SetupWizard.js");

	return new Promise<VendorId[] | null>((resolve) => {
		let picked: VendorId[] | null = null;
		const { waitUntilExit } = render(
			React.createElement(SetupWizard, {
				onComplete: (result: { vendors: VendorId[] }) => {
					picked = result.vendors;
					resolve(picked);
				},
			}),
		);
		// Quitting mid-wizard (q / ctrl-c) resolves null — leave vendors untouched.
		waitUntilExit().then(() => resolve(picked));
	});
}

/** Fallback consola-prompt wizard for non-interactive terminals */
async function runConsolaWizard(): Promise<VendorId[] | null> {
	consola.box("ImmorTerm Setup");
	consola.info("");

	// Step 1: Theme
	consola.info("");
	consola.info(pc.bold("Theme"));
	consola.info("  Choose a color theme for your status bar and CLI.");
	consola.info(`  ${pc.dim("You can change this anytime via: immorterm config set theme <name>")}`);

	const { THEME_NAMES, THEME_DESCRIPTIONS } = await import("../ui/banner.js");
	const themeChoice = await consola.prompt("Theme:", {
		type: "select",
		options: THEME_NAMES.map((name: string) => ({
			value: name,
			label: `${name} — ${(THEME_DESCRIPTIONS as Record<string, string>)[name] ?? ""}`,
		})),
		initial: "Purple Haze",
	});

	// Step 2: Service selection
	consola.info("");
	consola.info(pc.bold("Choose your services:"));
	consola.info(`Learn about all features: ${pc.cyan("https://immorterm.com/features")}`);
	consola.info("");

	// Memory service explanation
	consola.info(pc.bold("AI Memory (native binary)"));
	consola.info("  Your AI remembers every decision, architectural choice, and lesson learned.");
	consola.info("  No more re-explaining context when you start a new session.");
	consola.info(`  ${pc.dim("Runs locally as a native binary (~15 MB). Your data never leaves your machine.")}`);
	const enableMemory = await consola.prompt("Enable AI Memory?", {
		type: "confirm",
		initial: true,
	});
	consola.info("");

	// MCP Gateway explanation
	consola.info(pc.bold("MCP Gateway (shared MCP proxy)"));
	consola.info("  Routes all AI tool calls through a single persistent HTTP gateway.");
	consola.info("  Eliminates per-session MCP process spawning — faster tool responses.");
	consola.info(`  ${pc.dim("Connects your IDE tools through one shared server instead of many.")}`);
	const enableGateway = await consola.prompt("Enable MCP Gateway?", {
		type: "confirm",
		initial: false,
	});

	// Step 3: AI tools (per-project). Detected tools are pre-ticked so the
	// common case is a single enter.
	consola.info("");
	consola.info(pc.bold("Which AI tools should ImmorTerm hook into?"));
	consola.info(`  ${pc.dim("Applies to this project. Enabling a tool writes its hook config into the project root.")}`);
	const probes = detectVendors();
	const detected = detectedVendorIds(probes);
	const initialVendors = detected.length > 0 ? detected : (["claudeCode"] as VendorId[]);
	const vendorChoice = await consola.prompt("AI tools:", {
		type: "multiselect",
		required: false,
		options: [...probes]
			.sort((a, b) => Number(b.configured) - Number(a.configured) || Number(b.installed) - Number(a.installed))
			.map((p) => ({
				value: p.id,
				label: `${p.display}${p.configured ? " ✓ detected" : p.installed ? " (installed)" : ""}`,
			})),
		initial: initialVendors as unknown as string[],
	});
	// consola's multiselect resolves to the selected values, though its type
	// says option objects — normalize both shapes.
	const selectedVendors: VendorId[] = Array.isArray(vendorChoice)
		? (vendorChoice as unknown[]).map((v) =>
				(typeof v === "string" ? v : (v as { value: string }).value) as VendorId,
			)
		: initialVendors;

	// Step 4: License key
	consola.info("");
	const hasLicense = await consola.prompt("Do you have a license key?", {
		type: "confirm",
		initial: false,
	});

	let licenseResult: Awaited<ReturnType<typeof activateLicense>> | null = null;
	if (hasLicense) {
		const key = await consola.prompt("Enter your license key:", { type: "text" });
		if (key && typeof key === "string") {
			consola.start("Activating license...");
			licenseResult = await activateLicense(key);
			if (licenseResult.success) {
				consola.success(`License activated! (${licenseResult.license?.email ?? "Pro"})`);
			} else {
				consola.warn(`License activation failed: ${licenseResult.error}`);
				consola.info("Continuing with free tier. You can activate later via `immorterm license activate`.");
			}
		}
	}

	// Step 5: Write config
	ensureGlobalConfig();
	const config = readGlobalConfig();

	config.defaults.services.memory.enabled = enableMemory === true;
	config.defaults.services.mcpGateway.enabled = enableGateway === true;
	// One terminal now — the Rust engine. 'regular'/'both' remain readable for old configs.
	config.defaults.terminalMode = "ai";
	if (typeof themeChoice === 'string') {
		config.theme = themeChoice;
	}

	if (licenseResult?.success && licenseResult.license) {
		config.license.key = licenseResult.license.key ?? null;
		config.license.status = "active";
		config.license.customerEmail = licenseResult.license.email ?? null;
		config.license.expiresAt = licenseResult.license.expiresAt ?? null;
	}

	writeGlobalConfig(config);

	// Analytics
	await identify({ source: "init" });
	await track("cli_init_completed", {
		memory: enableMemory,
		gateway: enableGateway,
		hasLicense: licenseResult?.success ?? false,
		theme: typeof themeChoice === 'string' ? themeChoice : 'Purple Haze',
	});

	// Summary
	consola.info("");
	consola.success(pc.bold("ImmorTerm initialized!"));
	consola.info("");
	const themeLabel = typeof themeChoice === 'string' ? themeChoice : 'Purple Haze';
	consola.info(`  Config: ${pc.dim("~/.immorterm/config.json")}`);
	consola.info(`  Theme: ${pc.magenta(themeLabel)}`);
	consola.info(`  Memory: ${enableMemory ? pc.green("enabled") : pc.dim("disabled")}`);
	consola.info(`  Gateway: ${enableGateway ? pc.green("enabled") : pc.dim("disabled")}`);
	consola.info(`  License: ${licenseResult?.success ? pc.green("Pro") : pc.dim("Free")}`);
	consola.info(`  AI tools: ${formatVendorList(selectedVendors)}`);
	consola.info("");
	consola.info(`Next: ${pc.cyan("immorterm start")} to start services`);

	return selectedVendors;
}

/** Human-readable vendor list for the init summary. */
function formatVendorList(vendors: readonly VendorId[]): string {
	if (vendors.length === 0) return pc.dim("none");
	const byId = new Map(detectVendors().map((p) => [p.id, p.display]));
	return pc.green(vendors.map((v) => byId.get(v) ?? v).join(", "));
}

/** Non-interactive init — no prompts, write defaults + flags directly */
async function runNonInteractive(flags: InitFlags): Promise<VendorId[]> {
	const enableMemory = flags.memory ?? true;
	const enableGateway = flags.gateway ?? false;
	const theme = flags.theme ?? DEFAULT_THEME;
	const vendors = resolveVendorFlag(flags.vendors);

	ensureGlobalConfig();
	const config = readGlobalConfig();
	config.defaults.services.memory.enabled = enableMemory;
	config.defaults.services.mcpGateway.enabled = enableGateway;
	config.defaults.terminalMode = "ai";
	config.theme = theme;

	let licenseActivated = false;
	if (flags.licenseKey) {
		const result = await activateLicense(flags.licenseKey);
		if (result.success && result.license) {
			config.license.key = result.license.key ?? null;
			config.license.status = "active";
			config.license.customerEmail = result.license.email ?? null;
			config.license.expiresAt = result.license.expiresAt ?? null;
			licenseActivated = true;
		} else {
			consola.warn(`License activation failed: ${result.error}`);
		}
	}

	writeGlobalConfig(config);

	// Analytics (fire-and-forget)
	try {
		await identify({ source: "init" });
		await track("cli_init_completed", {
			memory: enableMemory,
			gateway: enableGateway,
			hasLicense: licenseActivated,
			theme,
			nonInteractive: true,
		});
	} catch { /* non-critical */ }

	consola.success(pc.bold("ImmorTerm initialized (non-interactive)"));
	consola.info(`  Config: ${pc.dim("~/.immorterm/config.json")}`);
	consola.info(`  Theme: ${pc.magenta(theme)}`);
	consola.info(`  Memory: ${enableMemory ? pc.green("enabled") : pc.dim("disabled")}`);
	consola.info(`  Gateway: ${enableGateway ? pc.green("enabled") : pc.dim("disabled")}`);
	consola.info(`  License: ${licenseActivated ? pc.green("Pro") : pc.dim("Free")}`);
	consola.info(`  AI tools: ${formatVendorList(vendors)}`);

	return vendors;
}

/** Install the VS Code extension for all init paths — non-fatal, always printed */
async function tryInstallExtension(): Promise<void> {
	try {
		await autoInstallExtension({
			info: (msg) => consola.info(msg),
			warn: (msg) => consola.warn(msg),
			error: (msg) => consola.error(msg),
		});
	} catch (e: any) {
		consola.warn(`VS Code extension install failed: ${e.message}`);
	}
}

/** If memory ended up enabled but the binary is missing, offer (TTY) or hint (non-TTY) to install it */
async function offerMemoryBinaryInstall(interactive: boolean): Promise<void> {
	const config = readGlobalConfig();
	if (!config.defaults.services.memory.enabled || findBinary()) return;

	if (!interactive) {
		consola.info(`Memory binary not found. Install it with: ${pc.cyan("immorterm memory install")}`);
		return;
	}

	const install = await consola.prompt(
		"AI Memory is enabled but the memory binary is missing. Download and install it now?",
		{ type: "confirm", initial: true },
	);
	if (install !== true) {
		consola.info(`Install later with: ${pc.cyan("immorterm memory install")}`);
		return;
	}

	try {
		await installMemoryBinary({
			info: (msg) => consola.info(msg),
			warn: (msg) => consola.warn(msg),
			error: (msg) => consola.error(msg),
		});
		consola.success("Memory binary installed.");
	} catch (e: any) {
		consola.warn(`Memory binary install failed: ${e.message}`);
		consola.info(`Retry with: ${pc.cyan("immorterm memory install")}`);
	}
}

/** If memory ended up enabled, install hooks into the current project.
 *  Without this, CLI-only users get a memory service that captures nothing.
 *
 *  `vendors` is the wizard's per-project selection; it must be persisted BEFORE
 *  the install so the installer reads it back and writes exactly those vendors'
 *  config files. Null = the user quit the wizard; leave any existing selection
 *  alone. */
function installHooksForCurrentProject(vendors: VendorId[] | null): void {
	const config = readGlobalConfig();
	if (!config.defaults.services.memory.enabled) return;
	const cwd = process.cwd();
	if (vendors) {
		try {
			setProjectVendors(cwd, vendors);
		} catch (e: any) {
			consola.warn(`Could not save AI tool selection: ${e.message}`);
		}
	}
	ensureProjectMemoryHooks(cwd);
}

/** Entry point — picks ink wizard, consola fallback, or non-interactive path */
export async function runInit(flags: InitFlags = {}): Promise<void> {
	const isTTY = process.stdin.isTTY && process.stdout.isTTY;
	const nonInteractive = !isTTY || flags.yes === true;

	let vendors: VendorId[] | null = null;
	if (nonInteractive) {
		// Piped/non-TTY stdin can't answer prompts — never enter a wizard
		vendors = await runNonInteractive(flags);
	} else {
		try {
			vendors = await runInkWizard();
		} catch {
			// Ink failed (missing deps, raw mode issue) — fallback
			vendors = await runConsolaWizard();
		}
	}

	// Post-wizard steps for ALL paths — non-fatal
	await tryInstallExtension();
	await offerMemoryBinaryInstall(!nonInteractive);
	installHooksForCurrentProject(vendors);
}

export const initCommand = defineCommand({
	meta: {
		name: "init",
		description: "Setup wizard — configure services and license",
	},
	args: {
		yes: {
			type: "boolean",
			description: "Non-interactive: skip prompts and accept defaults",
			alias: "y",
			default: false,
		},
		memory: {
			type: "boolean",
			description: "Enable AI Memory (non-interactive, default: true)",
		},
		gateway: {
			type: "boolean",
			description: "Enable MCP Gateway (non-interactive, default: false)",
		},
		theme: {
			type: "string",
			description: `Theme name (non-interactive, default: ${DEFAULT_THEME})`,
		},
		licenseKey: {
			type: "string",
			description: "License key to activate (non-interactive)",
		},
		vendors: {
			type: "string",
			description:
				"Comma-separated AI tools to hook into, e.g. --vendors=codex,claudeCode (non-interactive, default: auto-detected)",
		},
	},
	async run({ args }) {
		await runInit({
			yes: args.yes === true,
			memory: typeof args.memory === "boolean" ? args.memory : undefined,
			gateway: typeof args.gateway === "boolean" ? args.gateway : undefined,
			theme: typeof args.theme === "string" ? args.theme : undefined,
			licenseKey: typeof args.licenseKey === "string" ? args.licenseKey : undefined,
			vendors: typeof args.vendors === "string" ? args.vendors : undefined,
		});
	},
});
