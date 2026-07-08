/**
 * immorterm start [service] — Start all or specific services
 */

import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import { readGlobalConfig } from "@immorterm/config";
import { startMemory, startGateway, GATEWAY_PORT } from "@immorterm/services";
import { track } from "@immorterm/analytics";

const log = {
	info: (msg: string) => consola.info(msg),
	warn: (msg: string) => consola.warn(msg),
	error: (msg: string) => consola.error(msg),
};

export const startCommand = defineCommand({
	meta: {
		name: "start",
		description: "Start all or specific services",
	},
	args: {
		service: {
			type: "positional",
			description: "Service to start: memory, gateway (omit for all)",
			required: false,
		},
	},
	async run({ args }) {
		const service = args.service as string | undefined;
		const config = readGlobalConfig();

		if (!service || service === "memory") {
			if (!config.defaults.services.memory.enabled && !service) {
				consola.info(`Memory service is ${pc.dim("disabled")}. Enable with: immorterm enable memory`);
			} else {
				consola.start("Starting Memory...");
				const state = await startMemory(log);
				if (state.apiHealthy) {
					consola.success(
						`Memory ${pc.green("running")} (API: healthy, MCP: ${state.mcpHealthy ? "healthy" : "starting"})`,
					);
				} else {
					consola.error(`Memory failed to start: ${state.lastError ?? "unknown error"}`);
				}
			}
		}

		if (!service || service === "gateway") {
			if (!config.defaults.services.mcpGateway.enabled && !service) {
				consola.info(`MCP Gateway is ${pc.dim("disabled")}. Enable with: immorterm enable gateway`);
			} else {
				consola.start("Starting MCP Gateway...");
				const state = await startGateway(GATEWAY_PORT, log);
				if (state.healthy) {
					consola.success(
						`MCP Gateway ${pc.green("running")} (port ${state.port}, ${state.serverCount ?? 0} servers)`,
					);
				} else {
					consola.error(`MCP Gateway failed to start: ${state.lastError ?? "unknown error"}`);
				}
			}
		}

		await track("cli_start", { service: service ?? "all" });
	},
});
