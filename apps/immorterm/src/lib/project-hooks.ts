/**
 * CLI-side memory hook installation for the current project.
 *
 * Thin wrapper over @immorterm/services `installProjectHooks` (the shared
 * hook-installer core also used by the VS Code extension) that supplies the
 * CLI's static resource locations and prints what was installed.
 */

import * as path from "node:path";
import { fileURLToPath } from "node:url";
import consola from "consola";
import pc from "picocolors";
import { installProjectHooks } from "@immorterm/services";

/**
 * Candidate dirs containing `hooks/digest-llm-invoke.sh` and
 * `hooks/immorterm-notify.mjs`. Probed in order:
 * 1. dist/resources           — published npm package (build copies them in)
 * 2. ../../extension/resources — running dist/cli.mjs inside the repo
 * 3. ../../../extension/resources — dev run (`bun run src/main.ts`)
 */
export function hookResourceRoots(): string[] {
	const selfDir = path.dirname(fileURLToPath(import.meta.url));
	return [
		path.join(selfDir, "resources"),
		path.resolve(selfDir, "..", "..", "extension", "resources"),
		path.resolve(selfDir, "..", "..", "..", "extension", "resources"),
	];
}

/**
 * Install (or refresh) memory hooks for the project at `projectRoot`.
 * Idempotent; never clobbers non-immorterm hooks in .claude/settings.local.json.
 * Prints a summary of what was installed. Returns true on success.
 */
export function ensureProjectMemoryHooks(projectRoot: string): boolean {
	try {
		const res = installProjectHooks(projectRoot, {
			resourceRoots: hookResourceRoots(),
		});
		if (res.ok) {
			consola.success(`Memory hooks installed for this project ${pc.dim(`(${res.projectId})`)}`);
			consola.info(`  Hooks:    ${pc.dim(res.hooksDir)}`);
			consola.info(`  Settings: ${pc.dim(res.settingsPath)} ${pc.dim("(Claude Code hooks + MCP server)")}`);
			return true;
		}
		consola.warn(`Memory hook install failed. Retry with: ${pc.cyan("immorterm memory install")}`);
		return false;
	} catch (e: any) {
		consola.warn(`Memory hook install failed: ${e.message}`);
		consola.info(`Retry with: ${pc.cyan("immorterm memory install")}`);
		return false;
	}
}
