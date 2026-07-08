/**
 * immorterm disable <service> [--project .] — Disable a service
 */

import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import {
	readGlobalConfig,
	writeGlobalConfig,
	setServiceEnabled,
	getProjectId,
	ensureProjectIdentity,
} from "@immorterm/config";
import { track } from "@immorterm/analytics";

export const disableCommand = defineCommand({
	meta: {
		name: "disable",
		description: "Disable a service globally or per-project",
	},
	args: {
		service: {
			type: "positional",
			description: "Service: memory, gateway",
			required: true,
		},
		project: {
			type: "string",
			description: "Project path (for per-project config)",
			alias: "p",
		},
	},
	async run({ args }) {
		const service = normalizeServiceName(args.service);
		if (!service) {
			consola.error(`Unknown service: ${args.service}. Use: memory, gateway`);
			return;
		}

		if (args.project) {
			const projectPath = args.project === "." ? process.cwd() : args.project;
			const projectId = getProjectId(projectPath) || ensureProjectIdentity(projectPath).id;
			setServiceEnabled(projectPath, service, false, projectId);
			consola.success(`${pc.bold(args.service)} disabled for project: ${pc.dim(projectPath)}`);
		} else {
			const config = readGlobalConfig();
			config.defaults.services[service].enabled = false;
			writeGlobalConfig(config);
			consola.success(`${pc.bold(args.service)} disabled globally`);
		}

		await track("cli_disable", { service: args.service, scope: args.project ? "project" : "global" });
	},
});

function normalizeServiceName(input: string): "memory" | "mcpGateway" | null {
	switch (input.toLowerCase()) {
		case "memory":
			return "memory";
		case "gateway":
		case "mcpgateway":
		case "mcp-gateway":
			return "mcpGateway";
		default:
			return null;
	}
}
