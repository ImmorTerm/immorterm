/**
 * immorterm hooks [install|status] — project memory hook management.
 *
 * Thin wrapper over the shared hook-installer (via ensureProjectMemoryHooks)
 * so non-TS hosts — notably the Tauri app's Rust daemon — can wire memory
 * into a project by shelling out: `immorterm hooks install --project <dir>`.
 * Idempotent; installs into cwd unless --project is given.
 */

import { defineCommand } from "citty";
import consola from "consola";
import * as fs from "node:fs";
import * as path from "node:path";
import { areHooksInstalled } from "@immorterm/services";
import { ensureProjectMemoryHooks } from "../lib/project-hooks.js";

function resolveProjectRoot(project: unknown): string {
	const root = path.resolve(
		typeof project === "string" && project ? project : process.cwd(),
	);
	if (!fs.existsSync(root) || !fs.statSync(root).isDirectory()) {
		consola.error(`Not a directory: ${root}`);
		process.exit(1);
	}
	return root;
}

export const hooksCommand = defineCommand({
	meta: {
		name: "hooks",
		description: "Manage project memory hooks (install, status)",
	},
	args: {
		action: {
			type: "positional",
			description: "Action: install, status (default: status)",
			required: false,
		},
		project: {
			type: "string",
			description: "Project directory (default: current directory)",
		},
	},
	run({ args }) {
		const action = (args.action as string | undefined) || "status";
		const root = resolveProjectRoot(args.project);

		switch (action) {
			case "install": {
				const ok = ensureProjectMemoryHooks(root);
				process.exit(ok ? 0 : 1);
				break;
			}
			case "status": {
				const installed = areHooksInstalled(root);
				consola.info(
					installed
						? `Memory hooks installed (${root})`
						: `Memory hooks not installed (${root}) — run: immorterm hooks install`,
				);
				process.exit(installed ? 0 : 1);
				break;
			}
			default:
				consola.error(`Unknown action: ${action}`);
				consola.info("Usage: immorterm hooks [install|status] [--project <dir>]");
				process.exit(1);
		}
	},
});
