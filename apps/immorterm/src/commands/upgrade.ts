/**
 * immorterm upgrade [component] — Perform actual upgrades
 *
 * Per-component upgrade strategies:
 *   cli      → npm install -g immorterm@latest
 *   memory   → re-download latest GitHub release asset (daemon stop → replace → restart)
 *   extension → code --install-extension immorterm.immorterm-terminal
 *   ai       → no distribution channel yet (local builds only) — see docs/UPDATING.md
 *
 * Without a component argument, upgrades all outdated components.
 */

import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import * as fs from "node:fs";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import {
	EXTENSION_ID,
	getAllVersions,
	checkCliUpdate,
	getCliVersion,
	findBinary,
	installMemoryBinary,
	stopMemory,
	startMemory,
	MEMORY_BINARY,
} from "@immorterm/services";
import { readGlobalConfig, writeGlobalConfig, getGlobalConfigPath } from "@immorterm/config";
import type { ComponentVersion } from "@immorterm/services";

const execFileAsync = promisify(execFile);

const RELEASES_URL = "https://github.com/ImmorTerm/immorterm/releases";
const UPDATING_DOC_URL = "https://github.com/ImmorTerm/immorterm/blob/main/docs/UPDATING.md";

async function upgradeCli(dryRun: boolean): Promise<boolean> {
	if (dryRun) {
		consola.info(`  ${pc.dim("Would run:")} npm install -g immorterm@latest`);
		return true;
	}
	try {
		consola.start("Upgrading CLI via npm...");
		await execFileAsync("npm", ["install", "-g", "immorterm@latest"], { timeout: 120000 });
		consola.success("CLI upgraded");
		return true;
	} catch (error) {
		consola.error(`CLI upgrade failed: ${error}`);
		consola.info(`  Manual: ${pc.cyan("npm install -g immorterm@latest")}`);
		return false;
	}
}

export async function upgradeMemory(dryRun: boolean): Promise<boolean> {
	const log = {
		info: (msg: string) => consola.info(`  ${msg}`),
		warn: (msg: string) => consola.warn(`  ${msg}`),
		error: (msg: string) => consola.error(`  ${msg}`),
	};

	if (dryRun) {
		consola.info(
			`  ${pc.dim("Would:")} stop daemon → download latest release → replace ${MEMORY_BINARY} → restart`,
		);
		return true;
	}

	try {
		if (findBinary()) {
			// Stop ONLY the pid-file daemon before swapping the binary underneath it
			consola.start("Stopping memory daemon...");
			await stopMemory(log);
		}

		consola.start("Downloading latest memory binary...");
		await installMemoryBinary(log);

		consola.start("Starting memory daemon...");
		const state = await startMemory(log);
		if (state.apiHealthy) {
			consola.success("Memory daemon running and healthy");
		} else {
			consola.warn(`Memory started but health check pending: ${state.lastError ?? "unknown"}`);
		}
		return true;
	} catch (error) {
		consola.error(`Memory upgrade failed: ${error}`);
		consola.info(`  Manual download: ${pc.cyan(RELEASES_URL)}`);
		// Best effort: bring the old daemon back so a failed download doesn't leave memory down
		await startMemory(log).catch(() => {});
		return false;
	}
}

async function upgradeExtension(dryRun: boolean): Promise<boolean> {
	if (dryRun) {
		consola.info(`  ${pc.dim("Would run:")} code --install-extension ${EXTENSION_ID}`);
		return true;
	}
	try {
		consola.start("Upgrading VS Code extension...");
		await execFileAsync("code", ["--install-extension", EXTENSION_ID, "--force"], { timeout: 60000 });
		consola.success("Extension upgraded (reload VS Code to activate)");
		return true;
	} catch (error) {
		consola.error(`Extension upgrade failed: ${error}`);
		consola.info(`  Manual: ${pc.cyan(`code --install-extension ${EXTENSION_ID}`)}`);
		return false;
	}
}

/**
 * Startup staleness check (wires the dormant AutoUpdateConfig).
 * Prints an upgrade hint at most once per autoUpdate.checkIntervalHours.
 * Cheap on every other invocation: one config read, no network.
 */
export async function maybePrintUpgradeHint(): Promise<void> {
	try {
		// Never create a config file for users who haven't run `immorterm init`
		if (!fs.existsSync(getGlobalConfigPath())) return;
		const config = readGlobalConfig();
		const autoUpdate = config.autoUpdate;
		if (!autoUpdate?.enabled) return;
		const last = autoUpdate.lastCheckedAt ? Date.parse(autoUpdate.lastCheckedAt) : 0;
		if (Date.now() - last < autoUpdate.checkIntervalHours * 3_600_000) return;
		// Stamp before the network call so a slow/failed check can't retry every run
		autoUpdate.lastCheckedAt = new Date().toISOString();
		writeGlobalConfig(config);
		const latest = await checkCliUpdate();
		if (latest) {
			consola.info(
				`Update available: immorterm ${getCliVersion()} → ${latest}. Run ${pc.cyan("npm install -g immorterm@latest")}`,
			);
		}
	} catch {
		// The hint must never break a real command
	}
}

export async function upgradeAi(_dryRun: boolean): Promise<boolean> {
	// Honest: there is no distribution channel yet. No ai-* GitHub release has
	// ever been published, npm @immorterm/ai is 404, and no brew formula exists.
	consola.warn("immorterm-ai is not yet distributable — no public release channel exists.");
	consola.info(`  Status and channels: ${pc.cyan(UPDATING_DOC_URL)}`);
	return false;
}

type UpgradeFn = (dryRun: boolean) => Promise<boolean>;

const UPGRADE_MAP: Record<string, UpgradeFn> = {
	CLI: upgradeCli,
	Extension: upgradeExtension,
	Memory: upgradeMemory,
	"AI Binary": upgradeAi,
};

const COMPONENT_ALIASES: Record<string, string> = {
	cli: "CLI",
	extension: "Extension",
	ext: "Extension",
	memory: "Memory",
	mem: "Memory",
	ai: "AI Binary",
	"ai-binary": "AI Binary",
};

export const upgradeCommand = defineCommand({
	meta: {
		name: "upgrade",
		description: "Upgrade ImmorTerm components (or check for updates)",
	},
	args: {
		component: {
			type: "positional",
			description: "Component to upgrade: cli, memory, extension, ai (omit for all)",
			required: false,
		},
		"dry-run": {
			type: "boolean",
			description: "Show what would be upgraded without making changes",
			default: false,
		},
		force: {
			type: "boolean",
			description: "Upgrade even if already up to date",
			default: false,
		},
	},
	async run({ args }) {
		const dryRun = args["dry-run"] as boolean;
		const force = args.force as boolean;
		const component = args.component as string | undefined;

		if (dryRun) {
			consola.info(pc.dim("(dry-run mode — no changes will be made)"));
			consola.info("");
		}

		// Resolve target component
		const targetName = component ? COMPONENT_ALIASES[component.toLowerCase()] : undefined;
		if (component && !targetName) {
			consola.error(`Unknown component: ${component}`);
			consola.info(`Valid components: ${Object.keys(COMPONENT_ALIASES).join(", ")}`);
			return;
		}

		consola.start("Checking for updates...");
		const versions = await getAllVersions();

		// Filter to target component if specified
		const targets = targetName
			? versions.filter((v) => v.name === targetName)
			: versions;

		const toUpgrade = force
			? targets.filter((v) => v.current !== null) // Only upgrade installed components
			: targets.filter((v) => v.updateAvailable);

		if (toUpgrade.length === 0) {
			consola.success("All components are up to date!");
			for (const v of targets) {
				if (v.current) {
					consola.info(`  ${v.name.padEnd(12)} ${v.current} ${pc.green("✓")}`);
				} else {
					consola.info(`  ${v.name.padEnd(12)} ${pc.dim("not installed")}`);
				}
			}
			return;
		}

		consola.info("");
		consola.info(pc.bold("Updates available:"));
		for (const v of toUpgrade) {
			const arrow = v.latest ? `${pc.dim(v.current!)} → ${pc.green(v.latest)}` : v.current!;
			consola.info(`  ${v.name.padEnd(12)} ${arrow}`);
		}
		consola.info("");

		let succeeded = 0;
		let failed = 0;

		for (const v of toUpgrade) {
			const fn = UPGRADE_MAP[v.name];
			if (!fn) continue;

			const ok = await fn(dryRun);
			if (ok) succeeded++;
			else failed++;
		}

		consola.info("");
		if (dryRun) {
			consola.info(`${toUpgrade.length} component(s) would be upgraded`);
		} else if (failed === 0) {
			consola.success(`${succeeded} component(s) upgraded successfully`);
		} else {
			consola.warn(`${succeeded} upgraded, ${failed} failed`);
		}
	},
});
