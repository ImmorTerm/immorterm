/**
 * immorterm stop [service] — Stop all or specific services
 */

import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import { stopMemory, stopGateway, GATEWAY_PORT } from "@immorterm/services";
import { track } from "@immorterm/analytics";

const log = {
	info: (msg: string) => consola.info(msg),
	warn: (msg: string) => consola.warn(msg),
	error: (msg: string) => consola.error(msg),
};

export const stopCommand = defineCommand({
	meta: {
		name: "stop",
		description: "Stop all or specific services",
	},
	args: {
		service: {
			type: "positional",
			description: "Service to stop: memory, gateway (omit for all)",
			required: false,
		},
	},
	async run({ args }) {
		const service = args.service as string | undefined;

		if (!service || service === "memory") {
			consola.start("Stopping Memory...");
			await stopMemory(log);
			consola.success(`Memory ${pc.dim("stopped")}`);
		}

		if (!service || service === "gateway") {
			consola.start("Stopping MCP Gateway...");
			await stopGateway(GATEWAY_PORT, log);
			consola.success(`MCP Gateway ${pc.dim("stopped")}`);
		}

		await track("cli_stop", { service: service ?? "all" });
	},
});
