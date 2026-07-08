/**
 * immorterm status — Show service states, versions, license, and tier info
 */

import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import { readGlobalConfig } from "@immorterm/config";
import {
	refreshMemoryState,
	checkGatewayHealth,
	getAllVersions,
	GATEWAY_PORT,
} from "@immorterm/services";
import { renderBanner, resolveTheme } from "../ui/banner.js";
import { isPro, getLimits, getTierLabel } from "../feature-gate.js";

export async function runStatus(): Promise<void> {
	for (const line of renderBanner(resolveTheme())) {
		console.log(line);
	}

	const config = readGlobalConfig();
	const limits = getLimits();

	// Memory
	if (config.defaults.services.memory.enabled) {
		const mem = await refreshMemoryState();
		const memIcon = mem.apiHealthy ? pc.green("●") : mem.running ? pc.yellow("●") : pc.red("●");
		const mcpTag = mem.mcpHealthy ? pc.green("MCP ok") : pc.dim("MCP off");
		const retentionTag = !isPro() ? pc.dim(` (${limits.memoryRetentionHours}h retrieval)`) : "";
		consola.info(
			`${memIcon} Memory: ${mem.apiHealthy ? "healthy" : mem.running ? "starting" : "stopped"} ${mcpTag}${retentionTag}`,
		);
	} else {
		consola.info(`${pc.dim("○")} Memory: ${pc.dim("disabled")}`);
	}

	// Gateway
	if (config.defaults.services.mcpGateway.enabled) {
		const gw = await checkGatewayHealth(GATEWAY_PORT);
		const gwIcon = gw.healthy ? pc.green("●") : pc.red("●");
		const serverLimit = !isPro() ? pc.dim(` (max ${limits.maxGatewayServers} servers)`) : "";
		const detail = gw.healthy
			? `${gw.serverCount ?? 0} servers, ${gw.activeChildren ?? 0} active${serverLimit}`
			: "stopped";
		consola.info(`${gwIcon} Gateway: ${detail}`);
	} else {
		consola.info(`${pc.dim("○")} Gateway: ${pc.dim("disabled")}`);
	}

	// Component Versions
	consola.info("");
	consola.info(pc.bold("Component Versions"));
	consola.info(pc.dim("──────────────────"));

	const versions = await getAllVersions();
	for (const v of versions) {
		const current = v.current ?? pc.dim("not installed");
		if (v.updateAvailable && v.latest) {
			consola.info(`  ${v.name.padEnd(12)} ${pc.dim(v.current!)}  → ${pc.green(v.latest)} available`);
		} else if (v.current) {
			consola.info(`  ${v.name.padEnd(12)} ${current}  ${pc.green("✓")} up to date`);
		} else {
			consola.info(`  ${v.name.padEnd(12)} ${pc.dim("not installed")}`);
		}
	}

	// Auto-update status
	const autoUpdate = config.autoUpdate ?? { enabled: true, checkIntervalHours: 24, lastCheckedAt: null };
	const autoUpdateLabel = autoUpdate.enabled ? pc.green("enabled") : pc.dim("disabled");
	const checkedAgo = autoUpdate.lastCheckedAt
		? formatTimeAgo(new Date(autoUpdate.lastCheckedAt))
		: "never";
	consola.info(`  Auto-update: ${autoUpdateLabel} (checked ${checkedAgo})`);

	// License + Tier
	consola.info("");
	const email = config.license.customerEmail ? ` (${config.license.customerEmail})` : "";
	consola.info(`License: ${getTierLabel()}${email}`);

	// Free tier limits summary
	if (!isPro()) {
		consola.info(
			pc.dim(`  ${limits.maxTerminals} terminals · ${limits.maxProjects} project · ${limits.memoryRetentionHours}h memory · ${limits.maxGatewayServers} MCP servers`),
		);
		consola.info(pc.dim(`  Upgrade: ${pc.cyan("https://immorterm.dev/pricing")}`));
	}
}

function formatTimeAgo(date: Date): string {
	const seconds = Math.floor((Date.now() - date.getTime()) / 1000);
	if (seconds < 60) return "just now";
	const minutes = Math.floor(seconds / 60);
	if (minutes < 60) return `${minutes}m ago`;
	const hours = Math.floor(minutes / 60);
	if (hours < 24) return `${hours}h ago`;
	const days = Math.floor(hours / 24);
	return `${days}d ago`;
}

export const statusCommand = defineCommand({
	meta: {
		name: "status",
		description: "Show service states, license, and active sessions",
	},
	run: runStatus,
});
