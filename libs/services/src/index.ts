// Types
export type {
	Logger,
	MemoryState,
	GatewayState,
	HealthResult,
} from "./types.js";

// Memory (native Rust binary)
export {
	MEMORY_PORT,
	MEMORY_BINARY,
	MEMORY_PID_FILE,
	MEMORY_DATA_DIR,
	MEMORY_LOG_FILE,
	MEMORY_MODELS_DIR,
	MEMORY_VERSION_FILE,
	getMemoryPort,
	findBinary,
	preflightMemoryBinary,
	isMemoryFirstBoot,
	tailMemoryLog,
	spawnMemoryDaemon,
	memoryAssetName,
	getInstalledMemoryTag,
	findLatestMemoryRelease,
	installMemoryBinary,
	checkMemoryHealth,
	checkMcpEndpointHealth,
	waitForMemory,
	startMemory,
	stopMemory,
	refreshMemoryState,
} from "./memory.js";

// Gateway
export {
	GATEWAY_PORT,
	GATEWAY_STATE_DIR,
	GATEWAY_STATE_FILE,
	getHealthUrl,
	checkGatewayHealth,
	startGateway,
	stopGateway,
} from "./gateway.js";

// Health
export { waitForHealthy } from "./health.js";

// Hooks
export type { HookHealthResult } from "./hooks.js";
export { checkHookHealth } from "./hooks.js";

// Hook installer (shared core — also consumed by the VS Code extension via relative import)
export type { HookInstallDeps } from "./hook-installer.js";
export {
	installMemoryHooks,
	areHooksInstalled,
	removeMemoryHooks,
	updateHooksIfNeeded,
	writeAllVendorConfigs,
	resolveVendors,
} from "./hook-installer.js";
export type { InstallProjectHooksResult } from "./project-hooks.js";
export { installProjectHooks } from "./project-hooks.js";

// Versions
export type { ComponentVersion } from "./versions.js";
export { getAllVersions, getCliVersion, checkCliUpdate, compareVersions } from "./versions.js";

// VS Code
export type { VsCodeBinary, VsCodeDetection, ExtensionInstallResult } from "./vscode.js";
export {
	detectVsCode,
	isExtensionInstalled,
	getExtensionVersion,
	installExtension,
	autoInstallExtension,
} from "./vscode.js";
