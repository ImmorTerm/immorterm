/**
 * Project hook installation — CLI-facing convenience over the shared
 * hook-installer core (./hook-installer.ts).
 *
 * Resolves everything `installMemoryHooks` needs from the canonical config
 * modules: project identity from `.immorterm/project.json` (minted if absent),
 * vendors from `.immorterm/config.json`, and the live memory port.
 *
 * NOT used by the VS Code extension — it has its own synced copy of
 * @immorterm/config and wires deps in
 * apps/extension/src/services/memory/hook-installer.ts.
 */

import * as path from "node:path";
import {
	defaultVendorsConfig,
	ensureProjectIdentity,
	readProjectConfig,
} from "@immorterm/config";
import { getMemoryPort } from "./memory.js";
import { installMemoryHooks, resolveVendors } from "./hook-installer.js";

export interface InstallProjectHooksResult {
	ok: boolean;
	projectId: string;
	/** Where the hook scripts were written */
	hooksDir: string;
	/** Claude Code settings file the hooks were registered in */
	settingsPath: string;
}

/**
 * Install (or refresh) ImmorTerm memory hooks for the project at `projectRoot`.
 * Idempotent — safe to re-run; never clobbers non-immorterm hooks in
 * .claude/settings.local.json.
 *
 * @param resourceRoots Candidate dirs containing `hooks/digest-llm-invoke.sh`
 *   and `hooks/immorterm-notify.mjs` (see HookInstallDeps.resourceRoots).
 */
export function installProjectHooks(
	projectRoot: string,
	opts?: { resourceRoots?: string[] },
): InstallProjectHooksResult {
	const { id: projectId } = ensureProjectIdentity(projectRoot);
	const config = readProjectConfig(projectRoot);
	const vendors = resolveVendors(config?.services?.vendors, defaultVendorsConfig());

	const ok = installMemoryHooks(projectRoot, projectId, {
		memoryPort: getMemoryPort(),
		vendors,
		resourceRoots: opts?.resourceRoots ?? [],
	});

	return {
		ok,
		projectId,
		hooksDir: path.join(projectRoot, ".immorterm", "hooks"),
		settingsPath: path.join(projectRoot, ".claude", "settings.local.json"),
	};
}
