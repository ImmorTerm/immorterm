/**
 * immorterm pro — Unified Pro command
 *
 * Free users:  Upgrade flow (pricing → checkout → auto-activate → celebrate)
 * Pro users:   Management dashboard (status, deactivate, etc.)
 *
 * Subcommands:
 *   (no args)   — upgrade flow or dashboard depending on tier
 *   status      — show license details + validate
 *   activate    — manual key activation
 *   deactivate  — remove license (transfer to another machine)
 *   dev         — toggle devTierOverride for development
 */

import { track } from "@immorterm/analytics";
import { IMMORTERM_GLOBAL_DIR, readGlobalConfig, writeGlobalConfig } from "@immorterm/config";
import { activateLicense, deactivateLicense, validateLicense } from "@immorterm/license";
import { tierDisplayLabel } from "@immorterm/types";
import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import { playCelebration } from "../ui/celebration.js";

// ── Constants ────────────────────────────────────────────────────

/** Dodo static checkout link; the API's /api/pricing serves the live URL,
 *  IMMORTERM_CHECKOUT_URL overrides both. */
const CHECKOUT_URL_OVERRIDE = process.env.IMMORTERM_CHECKOUT_URL ?? null;
const API_BASE = process.env.IMMORTERM_API_URL ?? "https://api.immorterm.com";
const PRO_PAGE_URL = "https://immorterm.com/pro";
const POLL_INTERVAL_MS = 5_000;
const POLL_TIMEOUT_MS = 3 * 60 * 1000; // 3 minutes

// ponytail: MCP Gateway is free forever — never list it as a paid benefit.
const PRO_BENEFITS = [
	["Unlimited memory", "no search limits, full session recall"],
	["Knowledge Packs", "digest books & docs into searchable memory"],
	["Graph search", "entity relationships across all memories"],
	["All themes", "every status bar theme unlocked"],
	["Priority support", "direct access to the team"],
];

// ── Helpers ──────────────────────────────────────────────────────

function buildCheckoutUrl(base: string, email?: string): string {
	const url = new URL(base);
	if (email) {
		// Dodo static-link prefill param
		url.searchParams.set("email", email);
	}
	return url.toString();
}

async function openInBrowser(url: string): Promise<boolean> {
	const { execFile } = await import("node:child_process");
	const { platform } = await import("node:os");

	const cmd = platform() === "darwin" ? "open" : platform() === "win32" ? "start" : "xdg-open";

	return new Promise((resolve) => {
		execFile(cmd, [url], (err) => resolve(!err));
	});
}

async function fetchPricing(): Promise<{
	pricing: string | null;
	checkoutUrl: string | null;
}> {
	try {
		const controller = new AbortController();
		const timeout = setTimeout(() => controller.abort(), 3000);
		const res = await fetch(`${API_BASE}/api/pricing`, {
			signal: controller.signal,
		});
		clearTimeout(timeout);

		if (!res.ok) return { pricing: null, checkoutUrl: null };
		const data = await res.json();
		return {
			pricing: data.priceFormatted ?? (data.price ? `$${data.price}/mo` : null),
			checkoutUrl: data.checkoutUrl ?? null,
		};
	} catch {
		return { pricing: null, checkoutUrl: null };
	}
}

/**
 * Polls until the subscription shows up (webhook race). The check
 * endpoint only confirms issuance with a masked preview — the full key
 * is delivered by email, so activation is always a manual paste.
 */
async function pollForPurchase(email: string): Promise<boolean> {
	const start = Date.now();
	while (Date.now() - start < POLL_TIMEOUT_MS) {
		try {
			const res = await fetch(`${API_BASE}/api/licenses/check?email=${encodeURIComponent(email)}`);
			if (res.ok) {
				const data = await res.json();
				if (data.found) {
					return true;
				}
			}
		} catch {
			// Network error — keep polling
		}
		await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS));
	}
	return false;
}

function isPro(config: ReturnType<typeof readGlobalConfig>): boolean {
	const override = config.license.devTierOverride;
	return config.license.status === "active" || (!!override && override !== "free");
}

async function persistActivation(
	key: string,
	email: string,
	result: Awaited<ReturnType<typeof activateLicense>>,
): Promise<void> {
	const config = readGlobalConfig();
	config.license.key = key;
	config.license.status = "active";
	config.license.tier = result.license?.tier ?? "pro";
	config.license.customerEmail = result.license?.email ?? email;
	config.license.expiresAt = result.license?.expiresAt ?? null;
	config.license.instanceId = result.license?.instanceId ?? null;
	config.license.productId = result.license?.productId?.toString() ?? null;
	config.license.lastValidatedAt = new Date().toISOString();
	writeGlobalConfig(config);
}

async function showActivationSuccess(config: ReturnType<typeof readGlobalConfig>): Promise<void> {
	await playCelebration(config.theme ?? undefined);

	consola.success(`License activated! Welcome to ${pc.green(pc.bold("Pro"))}!`);
	consola.info("");
	consola.info(pc.bold("What's unlocked:"));
	for (const [name] of PRO_BENEFITS) {
		consola.info(`  ${pc.green("\u2713")} ${name}`);
	}
	consola.info("");

	// Offer MCP Gateway enablement
	const freshConfig = readGlobalConfig();
	if (!freshConfig.defaults.services.mcpGateway.enabled) {
		const enableGateway = await consola.prompt("Enable MCP Gateway now?", {
			type: "confirm",
			initial: true,
		});
		if (enableGateway) {
			freshConfig.defaults.services.mcpGateway.enabled = true;
			writeGlobalConfig(freshConfig);
			consola.success("MCP Gateway enabled! It will start automatically in VS Code.");
		}
	}
}

// ── Upgrade Flow (free users) ────────────────────────────────────

async function runUpgradeFlow(): Promise<void> {
	// 1. Fetch dynamic pricing + checkout URL (non-blocking, with fallback)
	const { pricing, checkoutUrl: apiCheckoutUrl } = await fetchPricing();

	// 2. Show Pro benefits
	consola.info("");
	consola.info(pc.bold(pc.magenta("  Upgrade to ImmorTerm Pro")));
	if (pricing) {
		consola.info(pc.dim(`  ${pricing}`));
	}
	consola.info("");
	for (const [name, desc] of PRO_BENEFITS) {
		consola.info(`  ${pc.green("+")} ${pc.bold(name)} — ${desc}`);
	}
	consola.info("");
	consola.info(pc.dim(`  Learn more: ${pc.cyan(PRO_PAGE_URL)}`));
	consola.info("");

	// 3. Prompt for email
	const email = await consola.prompt("Enter your email (for checkout):", {
		type: "text",
		placeholder: "you@example.com",
	});

	if (typeof email !== "string" || !email.trim()) {
		consola.warn(`No email provided. You can still upgrade at ${pc.cyan(PRO_PAGE_URL)}`);
		return;
	}

	const trimmedEmail = email.trim();
	// Override env → API-served Dodo link → pro page (which carries the link)
	const checkoutBase = CHECKOUT_URL_OVERRIDE ?? apiCheckoutUrl ?? PRO_PAGE_URL;
	const checkoutUrl = buildCheckoutUrl(checkoutBase, trimmedEmail);

	// 4. Open checkout in browser
	consola.info("");
	consola.start("Opening checkout in your browser...");

	const opened = await openInBrowser(checkoutUrl);
	if (opened) {
		consola.success("Checkout opened in your browser.");
	} else {
		consola.warn("Could not open browser automatically.");
	}

	consola.info("");
	consola.info(pc.dim("If the page didn't open, copy this link:"));
	consola.info(`  ${pc.cyan(pc.underline(checkoutUrl))}`);
	consola.info("");

	// 5. Poll for purchase confirmation
	consola.start("Waiting for purchase confirmation (this may take a few minutes)...");
	consola.info(pc.dim("  Complete checkout in your browser. We'll confirm your purchase here."));
	consola.info("");

	const purchased = await pollForPurchase(trimmedEmail);

	if (purchased) {
		consola.success("Purchase confirmed! Your license key has been emailed to you.");
	} else {
		consola.info("");
		consola.info(pc.dim("No confirmation received yet. That's okay!"));
	}

	// 6. Manual key entry (the full key only ever arrives by email)
	consola.info("");
	consola.info(pc.bold("Your license key arrives via email — paste it below to activate."));
	consola.info("");

	const key = await consola.prompt("Paste your license key:", {
		type: "text",
		placeholder: "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX",
	});

	if (typeof key !== "string" || !key.trim()) {
		consola.info("");
		consola.info("No key entered. You can activate later with:");
		consola.info(`  ${pc.cyan("immorterm pro activate <key>")}`);
		return;
	}

	consola.start("Activating license...");
	const result = await activateLicense(key.trim());

	if (result.success) {
		await persistActivation(key.trim(), trimmedEmail, result);
		await showActivationSuccess(readGlobalConfig());
		await track("cli_pro_upgrade", { success: true, email: trimmedEmail, method: "manual" });
	} else {
		consola.error(`Activation failed: ${result.error}`);
		consola.info("");
		consola.info("You can try again with:");
		consola.info(`  ${pc.cyan("immorterm pro activate <key>")}`);
		await track("cli_pro_upgrade", { success: false, email: trimmedEmail, method: "manual" });
	}
}

// ── Pro Dashboard (pro users) ────────────────────────────────────

async function runProDashboard(): Promise<void> {
	const config = readGlobalConfig();
	const lic = config.license;

	consola.info("");
	consola.info(`  ${pc.green(pc.bold(`ImmorTerm ${tierDisplayLabel(lic.tier)}`))} — Active`);
	consola.info("");
	if (lic.customerEmail) {
		consola.info(`  Email:      ${lic.customerEmail}`);
	}
	if (lic.expiresAt) {
		consola.info(`  Renews:     ${lic.expiresAt}`);
	}
	if (lic.key) {
		consola.info(`  Key:        ${pc.dim(lic.key.slice(0, 8) + "...")}`);
	}
	if (lic.devTierOverride) {
		consola.info(`  ${pc.yellow("DEV OVERRIDE")}: ${pc.bold(lic.devTierOverride)}`);
	}
	consola.info("");

	const choice = await consola.prompt("What would you like to do?", {
		type: "select",
		options: [
			{ value: "status", label: "View full status", hint: "validate with the license server" },
			{ value: "deactivate", label: "Deactivate", hint: "transfer to another machine" },
			{ value: "pro-page", label: "Visit immorterm.com/pro", hint: "manage subscription" },
			{ value: "back", label: "Done" },
		],
	});

	if (choice === "status") {
		await runStatusCommand();
	} else if (choice === "deactivate") {
		await runDeactivateCommand();
	} else if (choice === "pro-page") {
		await openInBrowser(PRO_PAGE_URL);
		consola.success("Opened in your browser.");
	}
}

// ── Subcommand: status ───────────────────────────────────────────

async function runStatusCommand(): Promise<void> {
	const config = readGlobalConfig();
	const lic = config.license;

	consola.info(pc.bold("License Status"));
	consola.info("");
	if (lic.devTierOverride) {
		consola.info(`  ${pc.yellow("DEV OVERRIDE")}: ${pc.bold(lic.devTierOverride)}`);
		consola.info(`  ${pc.dim("Run")} immorterm pro dev off ${pc.dim("to use real license")}`);
		consola.info("");
	}
	consola.info(
		`  Tier:       ${lic.status === "active" ? pc.green(tierDisplayLabel(lic.tier)) : pc.dim("Free")}`,
	);
	consola.info(`  Email:      ${lic.customerEmail ?? pc.dim("\u2014")}`);
	consola.info(`  Expires:    ${lic.expiresAt ?? pc.dim("\u2014")}`);
	consola.info(`  Key:        ${lic.key ? pc.dim(lic.key.slice(0, 8) + "...") : pc.dim("\u2014")}`);
	consola.info(
		`  Instance:   ${lic.instanceId ? pc.dim(lic.instanceId.slice(0, 8) + "...") : pc.dim("\u2014")}`,
	);
	consola.info(`  Validated:  ${lic.lastValidatedAt ?? pc.dim("never")}`);

	if (lic.key) {
		consola.start("Validating license...");
		const result = await validateLicense(lic.key, lic.instanceId ?? undefined);
		if (result.success) {
			const freshConfig = readGlobalConfig();
			freshConfig.license.lastValidatedAt = new Date().toISOString();
			writeGlobalConfig(freshConfig);
			consola.success("License is valid.");
		} else {
			consola.warn("License validation failed. It may have expired or been revoked.");
		}
	}
}

// ── Subcommand: deactivate ───────────────────────────────────────

async function runDeactivateCommand(): Promise<void> {
	const config = readGlobalConfig();
	if (!config.license.key) {
		consola.info("No license is currently active.");
		return;
	}
	consola.start("Deactivating license...");
	const result = await deactivateLicense(
		config.license.key,
		config.license.instanceId ?? undefined,
	);
	if (result.success) {
		config.license.key = null;
		config.license.status = null;
		config.license.tier = null;
		config.license.customerEmail = null;
		config.license.expiresAt = null;
		config.license.instanceId = null;
		config.license.productId = null;
		config.license.lastValidatedAt = null;
		writeGlobalConfig(config);
		consola.success("License deactivated. Reverted to free tier.");
	} else {
		consola.error(`Deactivation failed: ${result.error}`);
	}
	await track("cli_pro_deactivate", { success: result.success });
}

// ── Subcommands ──────────────────────────────────────────────────

const activateSubcommand = defineCommand({
	meta: { name: "activate", description: "Activate a license key" },
	args: {
		key: {
			type: "positional",
			description: "License key",
			required: true,
		},
	},
	async run({ args }) {
		consola.start("Activating license...");
		const result = await activateLicense(args.key);
		if (result.success) {
			await persistActivation(args.key, "", result);
			await showActivationSuccess(readGlobalConfig());
		} else {
			consola.error(`Activation failed: ${result.error}`);
		}
		await track("cli_pro_activate", { success: result.success });
	},
});

const deactivateSubcommand = defineCommand({
	meta: { name: "deactivate", description: "Deactivate current license" },
	async run() {
		await runDeactivateCommand();
	},
});

const statusSubcommand = defineCommand({
	meta: { name: "status", description: "Show current license status" },
	async run() {
		await runStatusCommand();
	},
});

const devSubcommand = defineCommand({
	meta: { name: "dev", description: "Set a dev tier override (bypasses license validation)" },
	args: {
		tier: {
			type: "positional",
			description: '"pro", "memory-pro", "free", or "off" to clear',
			required: true,
		},
	},
	run({ args }) {
		const { existsSync, writeFileSync } = require("node:fs");
		const { join } = require("node:path");
		const sentinel = join(IMMORTERM_GLOBAL_DIR, ".dev");

		if (!existsSync(sentinel)) {
			writeFileSync(sentinel, "# Dev mode sentinel — delete this file to disable dev overrides\n");
			consola.info(`Created ${pc.cyan(sentinel)}`);
		}

		const config = readGlobalConfig();
		if (args.tier === "off" || args.tier === "clear") {
			config.license.devTierOverride = null;
			writeGlobalConfig(config);
			consola.success("Dev override cleared. Using real license validation.");
		} else {
			config.license.devTierOverride = args.tier;
			writeGlobalConfig(config);
			consola.success(
				`Dev override set: ${pc.bold(args.tier)}. All tier checks will return ${pc.yellow(args.tier)}.`,
			);
			consola.info(pc.dim(`  Run ${pc.cyan("immorterm pro dev off")} to revert.`));
		}
	},
});

// ── Main Command ─────────────────────────────────────────────────

export const proCommand = defineCommand({
	meta: {
		name: "pro",
		description: "Upgrade to Pro or manage your subscription",
	},
	subCommands: {
		status: statusSubcommand,
		activate: activateSubcommand,
		deactivate: deactivateSubcommand,
		dev: devSubcommand,
	},
	async run() {
		const config = readGlobalConfig();

		if (isPro(config)) {
			await runProDashboard();
		} else {
			await runUpgradeFlow();
		}
	},
});
