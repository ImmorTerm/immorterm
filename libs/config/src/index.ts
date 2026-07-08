/**
 * ImmorTerm Configuration — IDE-Independent
 *
 * Central source of truth for all ImmorTerm paths and config files.
 * Pure filesystem I/O — no VS Code API dependency.
 *
 * Directory layout:
 *   Global:   ~/.immorterm/          (scripts, config, service state)
 *   Project:  <workspace>/.immorterm/ (terminal state, per-project config)
 */

import * as path from "node:path";
import * as os from "node:os";
import * as fs from "node:fs";
import { randomUUID } from "node:crypto";

// ── Path Constants ─────────────────────────────────────────────────

/** Project-level ImmorTerm directory name */
export const IMMORTERM_PROJECT_DIR = ".immorterm";

/** Project-level terminals subdirectory (relative to workspace root) */
export const IMMORTERM_TERMINALS_DIR = ".immorterm/terminals";

/** Project-level restore JSON (relative to workspace root) */
export const IMMORTERM_RESTORE_JSON = ".immorterm/restore-terminals.json";

/** Global ImmorTerm directory */
export const IMMORTERM_GLOBAL_DIR = path.join(os.homedir(), ".immorterm");

/** Global scripts directory (shared across all projects) */
export const IMMORTERM_SCRIPTS_DIR = path.join(os.homedir(), ".immorterm", "scripts");

// ── Derived Path Helpers ───────────────────────────────────────────

/** Get absolute project .immorterm dir */
export function getProjectDir(workspacePath: string): string {
	return path.join(workspacePath, IMMORTERM_PROJECT_DIR);
}

/** Get absolute terminals dir for a project */
export function getTerminalsDir(workspacePath: string): string {
	return path.join(workspacePath, IMMORTERM_TERMINALS_DIR);
}

/** Get absolute logs dir for a project */
export function getLogsDir(workspacePath: string): string {
	return path.join(workspacePath, IMMORTERM_TERMINALS_DIR, "logs");
}

/** Get absolute pending dir for a project */
export function getPendingDir(workspacePath: string): string {
	return path.join(workspacePath, IMMORTERM_TERMINALS_DIR, "pending");
}

/** Get absolute renames dir for a project */
export function getRenamesDir(workspacePath: string): string {
	return path.join(workspacePath, IMMORTERM_TERMINALS_DIR, "renames");
}

/** Get absolute restore-terminals.json path */
export function getRestoreJsonPath(workspacePath: string): string {
	return path.join(workspacePath, IMMORTERM_RESTORE_JSON);
}

/** Get absolute path to a global script */
export function getGlobalScriptPath(scriptName: string): string {
	return path.join(IMMORTERM_SCRIPTS_DIR, scriptName);
}

/** Get absolute path to the per-project screenrc */
export function getProjectScreenrcPath(workspacePath: string): string {
	return path.join(workspacePath, IMMORTERM_PROJECT_DIR, "screenrc");
}

/** Get absolute path to the project config file */
export function getProjectConfigPath(workspacePath: string): string {
	return path.join(workspacePath, IMMORTERM_PROJECT_DIR, "config.json");
}

/** Get absolute path to the global config file */
export function getGlobalConfigPath(): string {
	return path.join(IMMORTERM_GLOBAL_DIR, "config.json");
}

// ── Config Types ───────────────────────────────────────────────────

export interface ServiceConfig {
	enabled: boolean;
}

export interface LicenseConfig {
	key: string | null;
	instanceId: string | null;
	status: string | null;
	/** The resolved tier name (e.g. "free", "pro", "team"). Derived from LS variant/product. */
	tier: string | null;
	expiresAt: string | null;
	lastValidatedAt: string | null;
	productId: string | null;
	variantId: string | null;
	customerEmail: string | null;
	/** Dev override — when set, bypasses all LS validation. Set via `immorterm license dev <tier>`. */
	devTierOverride: string | null;
}

export interface AutoUpdateConfig {
	/** Whether auto-update is enabled (default: true) */
	enabled: boolean;
	/** How often to check for updates, in hours (default: 24) */
	checkIntervalHours: number;
	/** ISO timestamp of last update check, or null if never checked */
	lastCheckedAt: string | null;
}

export interface AppearanceConfig {
	borderEnabled: boolean;
	borderOpacity: number;
	statusBarEnabled: boolean;
	statusBarAnimations: boolean;
	statusBarMode: 'always' | 'auto' | 'hidden';
}

export interface GlobalConfig {
	version: number;
	license: LicenseConfig;
	telemetry?: boolean;
	theme?: string;
	autoUpdate?: AutoUpdateConfig;
	appearance?: AppearanceConfig;
	defaults: {
		terminalMode?: 'regular' | 'ai' | 'both';
		services: {
			memory: ServiceConfig;
			mcpGateway: ServiceConfig;
			graph: ServiceConfig;
		};
	};
}

export interface MemoryServiceConfig extends ServiceConfig {
	graph: boolean;
}

/** Per-vendor enable flag. opt-OUT (default true). */
export interface VendorConfig {
	enabled: boolean;
}

/** Vendor identifier — keys of `services.vendors` in v3 ProjectConfig. */
export type VendorId =
	| 'claudeCode'
	| 'codex'
	| 'cursor'
	| 'windsurf'
	| 'cline'
	| 'opencode'
	| 'gemini'
	| 'aider'
	| 'copilot';

/** Digest LLM provider+model selection. Optional in v3 — wizard fills it. */
export interface DigestConfig {
	/** "anthropic-cli" | "anthropic-api" | "openai-api" | "gemini-api" | "ollama" | "llm-cli" */
	provider: string;
	/** Model identifier — e.g. "claude-sonnet-4-7", "gpt-4o-mini". */
	model: string;
}

/** Per-vendor map for v3 ProjectConfig.services.vendors. */
export interface VendorsConfig {
	claudeCode: VendorConfig;
	codex: VendorConfig;
	cursor: VendorConfig;
	windsurf: VendorConfig;
	cline: VendorConfig;
	opencode: VendorConfig;
	gemini: VendorConfig;
	aider: VendorConfig;
	copilot: VendorConfig;
}

export interface ProjectConfig {
	version: number;
	projectId: string;
	enabled?: boolean | null; // null/absent = unset (prompt), true = on, false = off
	theme?: string;
	/** Project-default AI character ID (key of CHARACTER_DEFS in menu-data).
	 * Falls through to "default" when absent. Per-session overrides live in
	 * the Rust daemon's registry.json, not here. */
	speakMode?: string;
	terminalMode?: 'regular' | 'ai' | 'both';
	services: {
		memory: MemoryServiceConfig;
		mcpGateway: ServiceConfig;
		/** v3+ — per-vendor enable flags. All default `enabled: true` (opt-OUT model).
		 * Installer writes per-vendor config files only when the flag is true. */
		vendors: VendorsConfig;
		/** v3+ — digest LLM selection. Optional; populated by first-run wizard.
		 * When absent, digester auto-detects (`claude` on PATH → anthropic-cli, etc.). */
		digest?: DigestConfig;
	};
}

// ── Default Configs ────────────────────────────────────────────────

export function defaultGlobalConfig(): GlobalConfig {
	return {
		version: 1,
		license: {
			key: null,
			instanceId: null,
			status: null,
			tier: null,
			expiresAt: null,
			lastValidatedAt: null,
			productId: null,
			variantId: null,
			customerEmail: null,
			devTierOverride: null,
		},
		autoUpdate: {
			enabled: true,
			checkIntervalHours: 24,
			lastCheckedAt: null,
		},
		appearance: {
			borderEnabled: true,
			borderOpacity: 1.0,
			statusBarEnabled: true,
			statusBarAnimations: true,
			statusBarMode: 'always' as const,
		},
		defaults: {
			services: {
				memory: { enabled: false },
				mcpGateway: { enabled: false },
				graph: { enabled: false },
			},
		},
	};
}

/**
 * Claude Code only by default (opt-IN model). Other vendors write config
 * dirs into the project root (`.cursor/`, `.windsurf/`, `.clinerules/`,
 * `opencode.json`, …) — polluting every project for tools the user may not
 * run. Enable per-project via `services.vendors.<vendor>.enabled`.
 */
export function defaultVendorsConfig(): VendorsConfig {
	return {
		claudeCode: { enabled: true },
		codex: { enabled: false },
		cursor: { enabled: false },
		windsurf: { enabled: false },
		cline: { enabled: false },
		opencode: { enabled: false },
		gemini: { enabled: false },
		aider: { enabled: false },
		copilot: { enabled: false },
	};
}

export function defaultProjectConfig(projectId: string): ProjectConfig {
	return {
		version: 3,
		projectId,
		services: {
			memory: { enabled: false, graph: false },
			mcpGateway: { enabled: false },
			vendors: defaultVendorsConfig(),
			// digest is intentionally omitted — wizard fills it in (Task 11).
		},
	};
}

// ── Config Read/Write ──────────────────────────────────────────────

/** Read global config from ~/.immorterm/config.json */
export function readGlobalConfig(): GlobalConfig {
	const configPath = getGlobalConfigPath();
	try {
		if (fs.existsSync(configPath)) {
			const raw = fs.readFileSync(configPath, "utf-8");
			return { ...defaultGlobalConfig(), ...JSON.parse(raw) };
		}
	} catch {
		// Corrupted or unreadable — return defaults
	}
	return defaultGlobalConfig();
}

/** Write global config to ~/.immorterm/config.json */
export function writeGlobalConfig(config: GlobalConfig): void {
	const configPath = getGlobalConfigPath();
	fs.mkdirSync(IMMORTERM_GLOBAL_DIR, { recursive: true });
	fs.writeFileSync(configPath, JSON.stringify(config, null, 2) + "\n", "utf-8");
	// Secure the config file (contains license keys)
	fs.chmodSync(configPath, 0o600);
}

/** Read per-project config from <workspace>/.immorterm/config.json, migrating v1 → v2 → v3 */
export function readProjectConfig(workspacePath: string): ProjectConfig | null {
	const configPath = getProjectConfigPath(workspacePath);
	try {
		if (fs.existsSync(configPath)) {
			const raw = fs.readFileSync(configPath, "utf-8");
			const parsed = JSON.parse(raw);

			// v1 → v2 migration: graph was top-level service, now nested under memory
			let migrated = parsed;
			if (!migrated.version || migrated.version < 2) {
				const graphEnabled = migrated.services?.graph?.enabled ?? false;
				migrated = {
					version: 2,
					projectId: migrated.projectId ?? "",
					enabled: migrated.enabled ?? undefined,
					theme: migrated.theme ?? migrated.statusBarTheme ?? undefined,
					services: {
						memory: {
							enabled: migrated.services?.memory?.enabled ?? false,
							graph: graphEnabled,
						},
						mcpGateway: {
							enabled: migrated.services?.mcpGateway?.enabled ?? false,
						},
					},
				};
			}

			// v2 → v3 migration: add per-vendor enable flags (all default true) and
			// leave digest undefined so the wizard fills it. Memory/mcpGateway opt-in
			// defaults are preserved verbatim — we only add the new vendors map.
			let didMigrate = migrated !== parsed;
			if (!migrated.version || migrated.version < 3) {
				migrated = {
					...migrated,
					version: 3,
					services: {
						...migrated.services,
						vendors: migrated.services?.vendors ?? defaultVendorsConfig(),
						// preserve existing digest if a beta config already had it
						...(migrated.services?.digest ? { digest: migrated.services.digest } : {}),
					},
				};
				didMigrate = true;
			}

			if (didMigrate) {
				writeProjectConfig(workspacePath, migrated as ProjectConfig);
			}

			return migrated as ProjectConfig;
		}
	} catch {
		// Corrupted or unreadable
	}
	return null;
}

/** Write per-project config to <workspace>/.immorterm/config.json */
export function writeProjectConfig(workspacePath: string, config: ProjectConfig): void {
	const dir = getProjectDir(workspacePath);
	fs.mkdirSync(dir, { recursive: true });
	const configPath = getProjectConfigPath(workspacePath);
	fs.writeFileSync(configPath, JSON.stringify(config, null, 2) + "\n", "utf-8");
}

// ── Merged Service Checks ──────────────────────────────────────────

/**
 * Check if a service is enabled for a project.
 * Per-project config overrides global defaults.
 */
export function isServiceEnabled(
	workspacePath: string,
	service: "memory" | "mcpGateway" | "graph",
): boolean {
	const projectConfig = readProjectConfig(workspacePath);

	// graph is nested under memory in v2
	if (service === "graph") {
		if (projectConfig?.services?.memory) {
			return (projectConfig.services.memory as MemoryServiceConfig).graph ?? false;
		}
		const globalConfig = readGlobalConfig();
		return globalConfig.defaults.services.graph?.enabled ?? false;
	}

	// memory and mcpGateway are top-level services
	if (projectConfig?.services?.[service]) {
		return projectConfig.services[service].enabled;
	}

	const globalConfig = readGlobalConfig();
	return globalConfig.defaults.services[service]?.enabled ?? false;
}

/**
 * Check if a per-vendor hook integration is enabled for a project (v3+).
 *
 * Vendors default to `enabled: true` (opt-OUT). Users can disable individual
 * vendors via per-project config. Mirrors `isServiceEnabled` semantics:
 * project-overrides-global with a sane default fallback.
 */
export function isVendorEnabled(workspacePath: string, vendor: VendorId): boolean {
	const projectConfig = readProjectConfig(workspacePath);
	const vendorEntry = projectConfig?.services?.vendors?.[vendor];
	if (vendorEntry && typeof vendorEntry.enabled === "boolean") {
		return vendorEntry.enabled;
	}
	// Default for any v3+ project that somehow lacks the vendor key: enabled.
	return true;
}

/** Get the project ID from config, or empty string if not configured */
export function getProjectId(workspacePath: string): string {
	const projectConfig = readProjectConfig(workspacePath);
	return projectConfig?.projectId ?? "";
}

/** Get absolute path to the canonical project identity file */
export function getProjectJsonPath(workspacePath: string): string {
	return path.join(workspacePath, IMMORTERM_PROJECT_DIR, "project.json");
}

/**
 * Read the canonical project identity from <workspace>/.immorterm/project.json,
 * minting it (tmp file + rename) when absent. The Rust daemon reuses an existing
 * project.json, so minting here is compatible with all writers.
 * Never returns a placeholder id — "default" is a banned identity literal.
 */
export function ensureProjectIdentity(workspacePath: string): { id: string; name: string } {
	const jsonPath = getProjectJsonPath(workspacePath);
	try {
		const parsed = JSON.parse(fs.readFileSync(jsonPath, "utf-8"));
		if (typeof parsed?.id === "string" && parsed.id) {
			return {
				id: parsed.id,
				name: typeof parsed.name === "string" ? parsed.name : path.basename(workspacePath),
			};
		}
	} catch {
		// Absent or corrupt — mint below
	}
	const identity = { id: randomUUID(), name: path.basename(workspacePath) };
	fs.mkdirSync(getProjectDir(workspacePath), { recursive: true });
	const tmpPath = `${jsonPath}.tmp.${process.pid}`;
	fs.writeFileSync(tmpPath, JSON.stringify(identity, null, 2) + "\n", "utf-8");
	fs.renameSync(tmpPath, jsonPath);
	return identity;
}

/** Get current license status from global config */
export function getLicenseStatus(): LicenseConfig {
	return readGlobalConfig().license;
}

/**
 * Ensure the global config file exists with default values.
 * Safe to call multiple times — won't overwrite existing config.
 */
export function ensureGlobalConfig(): void {
	const configPath = getGlobalConfigPath();
	if (!fs.existsSync(configPath)) {
		writeGlobalConfig(defaultGlobalConfig());
	}
}

/**
 * Create or update per-project config.
 * If config already exists, only updates the projectId (preserves user settings).
 */
export function ensureProjectConfig(workspacePath: string, projectId: string): void {
	const existing = readProjectConfig(workspacePath);
	if (existing) {
		// Update projectId if it changed
		if (existing.projectId !== projectId) {
			existing.projectId = projectId;
			writeProjectConfig(workspacePath, existing);
		}
		return;
	}
	writeProjectConfig(workspacePath, defaultProjectConfig(projectId));
}

/**
 * Update a service flag in config.json and return the new state.
 * Writes to per-project config.
 */
export function setServiceEnabled(
	workspacePath: string,
	service: "memory" | "mcpGateway" | "graph",
	enabled: boolean,
	projectId: string,
): ProjectConfig {
	let config = readProjectConfig(workspacePath);
	if (!config) {
		config = defaultProjectConfig(projectId);
	}

	if (service === "graph") {
		config.services.memory.graph = enabled;
	} else {
		config.services[service].enabled = enabled;
	}

	writeProjectConfig(workspacePath, config);
	return config;
}

// ── Enabled State & Theme ───────────────────────────────────────────

/**
 * Get the enabled state from config.json.
 * Returns 'unset' if the user hasn't made a choice yet.
 */
export function getEnabledState(workspacePath: string): "unset" | "enabled" | "disabled" {
	const config = readProjectConfig(workspacePath);
	if (!config || config.enabled === undefined || config.enabled === null) {
		return "unset";
	}
	return config.enabled ? "enabled" : "disabled";
}

/**
 * Set the enabled state in config.json.
 * Pass null to reset to 'unset' (user will be prompted again).
 */
export function setEnabledState(
	workspacePath: string,
	enabled: boolean | null,
	projectId: string,
): void {
	let config = readProjectConfig(workspacePath);
	if (!config) {
		config = defaultProjectConfig(projectId);
	}
	config.enabled = enabled ?? undefined;
	writeProjectConfig(workspacePath, config);
}

/** Get the theme from config.json */
export function getTheme(workspacePath: string): string | undefined {
	const config = readProjectConfig(workspacePath);
	return config?.theme;
}

/** Set the theme in config.json */
export function setTheme(workspacePath: string, theme: string, projectId: string): void {
	let config = readProjectConfig(workspacePath);
	if (!config) {
		config = defaultProjectConfig(projectId);
	}
	config.theme = theme;
	writeProjectConfig(workspacePath, config);
}

/** Get the project-default AI character ID from config.json.
 * Returns undefined when no project default is set — callers should
 * fall through to the string "default". */
export function getSpeakMode(workspacePath: string): string | undefined {
	const config = readProjectConfig(workspacePath);
	return config?.speakMode;
}

/** Set the project-default AI character ID in config.json.
 * Per-session overrides go through the daemon's registry, not this fn. */
export function setSpeakMode(
	workspacePath: string,
	speakMode: string,
	projectId: string,
): void {
	let config = readProjectConfig(workspacePath);
	if (!config) {
		config = defaultProjectConfig(projectId);
	}
	config.speakMode = speakMode;
	writeProjectConfig(workspacePath, config);
}

/** Get the terminal mode from config.json */
export function getConfigTerminalMode(workspacePath: string): "regular" | "ai" | "both" | undefined {
	const config = readProjectConfig(workspacePath);
	return config?.terminalMode;
}

/** Set the terminal mode in config.json */
export function setConfigTerminalMode(
	workspacePath: string,
	mode: "regular" | "ai" | "both",
	projectId: string,
): void {
	let config = readProjectConfig(workspacePath);
	if (!config) {
		config = defaultProjectConfig(projectId);
	}
	config.terminalMode = mode;
	writeProjectConfig(workspacePath, config);
}

// ── Appearance ──────────────────────────────────────────────────────

const DEFAULT_APPEARANCE: AppearanceConfig = {
	borderEnabled: true,
	borderOpacity: 1.0,
	statusBarEnabled: true,
	statusBarAnimations: true,
	statusBarMode: 'always',
};

/** Get appearance settings from global config, with defaults for missing keys */
export function getAppearance(): AppearanceConfig {
	const config = readGlobalConfig();
	return { ...DEFAULT_APPEARANCE, ...config.appearance };
}

/** Merge partial appearance updates into global config */
export function updateAppearance(partial: Partial<AppearanceConfig>): void {
	const config = readGlobalConfig();
	config.appearance = { ...DEFAULT_APPEARANCE, ...config.appearance, ...partial };
	writeGlobalConfig(config);
}
