/**
 * immorterm license — License management subcommands
 */

import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import { readGlobalConfig, writeGlobalConfig, IMMORTERM_GLOBAL_DIR } from "@immorterm/config";
import {
	activateLicense,
	deactivateLicense,
	validateLicense,
	getLicenseStatus,
} from "@immorterm/license";
import { track } from "@immorterm/analytics";
import { tierDisplayLabel } from "@immorterm/types";
import { playCelebration } from "../ui/celebration.js";

const activateCommand = defineCommand({
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
			const config = readGlobalConfig();
			config.license.key = args.key;
			config.license.status = "active";
			config.license.tier = result.license?.tier ?? "pro";
			config.license.customerEmail = result.license?.email ?? null;
			config.license.expiresAt = result.license?.expiresAt ?? null;
			config.license.instanceId = result.license?.instanceId ?? null;
			config.license.productId = result.license?.productId?.toString() ?? null;
			config.license.lastValidatedAt = new Date().toISOString();
			writeGlobalConfig(config);
			const label = tierDisplayLabel(config.license.tier);
			consola.success(`License activated! ${pc.green(label)} (${result.license?.email ?? ""})`);

			await playCelebration(config.theme ?? undefined);

			// Show unlocked benefits and offer gateway enablement
			consola.info("");
			consola.info(pc.bold(`${label} unlocked! You now have access to:`));
			consola.info(`  ${pc.green("✓")} Unlimited memory search results`);
			consola.info(`  ${pc.green("✓")} Unlimited memory retention`);
			consola.info(`  ${pc.green("✓")} Full session recall (all sessions, not just the last one)`);
			if (config.license.tier !== "memory-pro") {
				consola.info(`  ${pc.green("✓")} Knowledge Packs (digest books & docs into searchable memory)`);
				consola.info(`  ${pc.green("✓")} Graph search (entity relationships across memories)`);
				consola.info(`  ${pc.green("✓")} All status bar themes`);
			}
			consola.info("");

			// Re-read config after write to get fresh state
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
		} else {
			consola.error(`Activation failed: ${result.error}`);
		}
		await track("cli_license_activate", { success: result.success });
	},
});

const deactivateCommand = defineCommand({
	meta: { name: "deactivate", description: "Deactivate current license" },
	async run() {
		const config = readGlobalConfig();
		if (!config.license.key) {
			consola.info("No license is currently active.");
			return;
		}
		consola.start("Deactivating license...");
		const result = await deactivateLicense(config.license.key, config.license.instanceId ?? undefined);
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
		await track("cli_license_deactivate", { success: result.success });
	},
});

const licenseStatusCommand = defineCommand({
	meta: { name: "status", description: "Show current license status" },
	async run() {
		const config = readGlobalConfig();
		const lic = config.license;

		consola.info(pc.bold("License Status"));
		consola.info("");
		if (lic.devTierOverride) {
			consola.info(`  ${pc.yellow("DEV OVERRIDE")}: ${pc.bold(lic.devTierOverride)}`);
			consola.info(`  ${pc.dim("Run")} immorterm license dev off ${pc.dim("to use real license")}`);
			consola.info("");
		}
		consola.info(
			`  Tier:       ${lic.status === "active" ? pc.green(tierDisplayLabel(lic.tier)) : pc.dim("Free")}`,
		);
		consola.info(`  Email:      ${lic.customerEmail ?? pc.dim("—")}`);
		consola.info(`  Expires:    ${lic.expiresAt ?? pc.dim("—")}`);
		consola.info(`  Key:        ${lic.key ? pc.dim(lic.key.slice(0, 8) + "...") : pc.dim("—")}`);
		consola.info(`  Instance:   ${lic.instanceId ? pc.dim(lic.instanceId.slice(0, 8) + "...") : pc.dim("—")}`);
		consola.info(`  Validated:  ${lic.lastValidatedAt ?? pc.dim("never")}`);

		// Validate if there's an active key
		if (lic.key) {
			consola.start("Validating license...");
			const result = await validateLicense(lic.key, lic.instanceId ?? undefined);
			if (result.success) {
				// Update lastValidatedAt on successful validation
				const freshConfig = readGlobalConfig();
				freshConfig.license.lastValidatedAt = new Date().toISOString();
				writeGlobalConfig(freshConfig);
				consola.success("License is valid.");
			} else {
				consola.warn("License validation failed. It may have expired or been revoked.");
			}
		}
	},
});

const devCommand = defineCommand({
	meta: { name: "dev", description: "Set a dev tier override (bypasses license validation)" },
	args: {
		tier: {
			type: "positional",
			description: 'Tier to simulate: "pro", "memory-pro", "free", or "off" to clear',
			required: true,
		},
	},
	run({ args }) {
		const { existsSync, writeFileSync } = require("node:fs");
		const { join } = require("node:path");
		const sentinel = join(IMMORTERM_GLOBAL_DIR, ".dev");

		// Ensure .dev sentinel exists (auto-create on first use)
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
			consola.info(pc.dim(`  Run ${pc.cyan("immorterm license dev off")} to revert.`));
		}
	},
});

export const licenseCommand = defineCommand({
	meta: {
		name: "license",
		description: "Manage your ImmorTerm license",
	},
	subCommands: {
		activate: activateCommand,
		deactivate: deactivateCommand,
		status: licenseStatusCommand,
		dev: devCommand,
	},
});
