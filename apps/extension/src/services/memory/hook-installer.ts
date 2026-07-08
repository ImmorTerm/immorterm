/**
 * Hook Installer — VS Code extension glue over the shared core.
 *
 * The actual installer (hook templates, stampOwner owner stamps, settings.json
 * registration, vendor configs, git trampoline) lives in
 * libs/services/src/hook-installer.ts and is shared with the npm CLI
 * (`immorterm init` / `immorterm memory install`). This module only wires the
 * extension-specific dependencies:
 *
 * - memoryPort  → native-memory-manager (reads ~/.immorterm/memory.state.json)
 * - vendors     → extension's synced copy of @immorterm/config
 * - resourceRoots → the extension's resources/ dir, probed relative to
 *   __dirname so it works for the bundled esbuild build (out/extension.js),
 *   the unbundled tsc build (out/services/memory/), and source-tree runs.
 *
 * Keep the public API identical to the pre-extraction module — extension.ts,
 * activation.ts, services/memory/index.ts and the vitest suites import it.
 */

import * as path from 'path';
import {
  installMemoryHooks as installMemoryHooksCore,
  updateHooksIfNeeded as updateHooksIfNeededCore,
  writeAllVendorConfigs as writeAllVendorConfigsCore,
  resolveVendors,
  areHooksInstalled,
  removeMemoryHooks,
  type HookInstallDeps,
} from '../../../../../libs/services/src/hook-installer';
import { getMemoryPort } from './native-memory-manager';
import { readProjectConfig, defaultVendorsConfig } from '../../utils/immorterm-config';

// Re-export the pieces consumed directly by tests and other modules.
export {
  areHooksInstalled,
  removeMemoryHooks,
  CURSOR_ADAPTER_SH,
  WINDSURF_ADAPTER_SH,
  CLINE_ADAPTER_SH,
  AIDER_POST_COMMIT_SH,
} from '../../../../../libs/services/src/hook-installer';

/**
 * Candidate resource roots (dirs containing hooks/digest-llm-invoke.sh and
 * hooks/immorterm-notify.mjs). The TS source lives at
 * apps/extension/src/services/memory/, but esbuild bundles everything into
 * <ext>/out/extension.js — so at runtime __dirname resolves to <ext>/out/ and
 * resources/ is one dir up. The extra candidates cover unbundled `tsc` builds
 * (out/services/memory/) and source-tree runs.
 */
function resourceRoots(): string[] {
  return [
    // bundled build (esbuild → out/extension.js)
    path.resolve(__dirname, '..', 'resources'),
    // unbundled tsc build (out/services/memory/hook-installer.js)
    path.resolve(__dirname, '..', '..', '..', 'resources'),
    // dev/source-tree fallback (running from src)
    path.resolve(__dirname, '..', '..', '..', '..', 'resources'),
  ];
}

/** Assemble the extension-side deps for the shared installer core. */
function extensionDeps(projectPath: string): HookInstallDeps {
  const config = readProjectConfig(projectPath);
  return {
    memoryPort: getMemoryPort(),
    vendors: resolveVendors(config?.services?.vendors, defaultVendorsConfig()),
    resourceRoots: resourceRoots(),
  };
}

/**
 * Install all memory hooks for a project.
 *
 * @param projectPath Path to the project (workspace folder)
 * @param projectId The stable project ID
 * @returns true if hooks were installed successfully
 */
export function installMemoryHooks(projectPath: string, projectId: string): boolean {
  return installMemoryHooksCore(projectPath, projectId, extensionDeps(projectPath));
}

/**
 * Update hooks if project ID has changed.
 *
 * @param projectPath Path to the project
 * @param projectId The current project ID
 * @returns true if hooks were updated
 */
export function updateHooksIfNeeded(projectPath: string, projectId: string): boolean {
  return updateHooksIfNeededCore(projectPath, projectId, extensionDeps(projectPath));
}

/**
 * Write per-vendor config files for every enabled vendor. Reads the project
 * config to find enabled vendors; if config is missing or lacks vendors
 * (pre-v3), defaults apply (see resolveVendors for the all-enabled reset rule).
 */
export function writeAllVendorConfigs(projectPath: string): string[] {
  const config = readProjectConfig(projectPath);
  return writeAllVendorConfigsCore(
    projectPath,
    resolveVendors(config?.services?.vendors, defaultVendorsConfig())
  );
}

export default {
  installMemoryHooks,
  areHooksInstalled,
  removeMemoryHooks,
  updateHooksIfNeeded,
};
