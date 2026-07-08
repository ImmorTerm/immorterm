/**
 * Session Enricher — Read registry.json and enrich sessions with log file metadata
 *
 * Reads ~/.immorterm/registry.json, stats log files for each session,
 * checks process liveness, and returns enriched session objects ready for API/TUI.
 */

import * as fs from "node:fs";
import * as path from "node:path";
import { IMMORTERM_GLOBAL_DIR } from "@immorterm/config";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/** Raw registry entry from registry.json (matches Rust RegistryEntry) */
export interface RegistryEntry {
	pid: number;
	name: string;
	window_id: string;
	display_name: string;
	project_dir: string;
	claude_session_id?: string | null;
	title_locked: boolean;
	title: string;
	logfile?: string | null;
	shell: string;
	created_at: number;
	session_type?: string | null;
	ws_port?: number | null;
	theme?: string | null;
	claude_transcript_path?: string | null;
	claude_stats?: {
		pid?: number | null;
		rss_kb: number;
		cpu_percent: number;
		start_time?: number | null;
		runtime_secs: number;
		model?: string | null;
		cost_usd?: number | null;
		context_pct?: number | null;
	} | null;
	structured_log_dir?: string | null;
}

export interface Registry {
	sessions: RegistryEntry[];
}

export interface LogFileInfo {
	exists: boolean;
	size: number;
	/** Number of snapshots (only for grid) */
	snapshots?: number;
	/** Number of events (only for ai) */
	events?: number;
}

export interface EnrichedSession {
	id: string;
	pid: number;
	displayName: string;
	title: string;
	projectDir: string;
	shell: string;
	createdAt: number;
	sessionType: string;
	status: "alive" | "dead";
	claudeSessionId?: string | null;
	claudeStats?: RegistryEntry["claude_stats"];
	theme?: string | null;
	isArchived?: boolean;
	archivedAt?: number;
	logs: {
		grid: LogFileInfo;
		cast: LogFileInfo;
		ai: LogFileInfo;
		raw: LogFileInfo;
	};
}

// ---------------------------------------------------------------------------
// Registry reading
// ---------------------------------------------------------------------------

function getRegistryPath(): string {
	return path.join(IMMORTERM_GLOBAL_DIR, "registry.json");
}

export function readRegistry(): Registry {
	const registryPath = getRegistryPath();
	try {
		if (fs.existsSync(registryPath)) {
			const raw = fs.readFileSync(registryPath, "utf-8");
			return JSON.parse(raw) as Registry;
		}
	} catch {
		// Corrupted or missing
	}
	return { sessions: [] };
}

// ---------------------------------------------------------------------------
// Process liveness check
// ---------------------------------------------------------------------------

function isProcessAlive(pid: number): boolean {
	try {
		process.kill(pid, 0);
		return true;
	} catch {
		return false;
	}
}

// ---------------------------------------------------------------------------
// Log file stat helper
// ---------------------------------------------------------------------------

function statLogFile(filePath: string): LogFileInfo {
	try {
		const s = fs.statSync(filePath);
		return { exists: true, size: s.size };
	} catch {
		return { exists: false, size: 0 };
	}
}

function countLines(filePath: string): number {
	try {
		const content = fs.readFileSync(filePath, "utf-8");
		return content.split("\n").filter((l) => l.trim().length > 0).length;
	} catch {
		return 0;
	}
}

// ---------------------------------------------------------------------------
// Session ID derivation
// ---------------------------------------------------------------------------

/** Derive a short, URL-safe session ID from the registry name field */
function deriveSessionId(entry: RegistryEntry): string {
	// Registry name format: "immorterm-ai-abc12345" or "12345-abcDEFgh"
	// Use window_id if it's clean, otherwise use name
	return entry.window_id || entry.name;
}

// ---------------------------------------------------------------------------
// Resolve log file paths for a session
// ---------------------------------------------------------------------------

function resolveLogPaths(entry: RegistryEntry): {
	grid: string;
	cast: string;
	ai: string;
	raw: string;
} {
	const baseLogDir = path.join(
		entry.project_dir,
		".immorterm",
		"terminals",
		"logs",
	);
	const logDir = entry.structured_log_dir || baseLogDir;
	const sessionName = entry.name;
	const windowId = entry.window_id;

	// 1. Per-session directory format: structured_log_dir contains grid.jsonl directly
	//    (Rust daemon and C binary now create {date}_{windowId}/ dirs)
	if (entry.structured_log_dir) {
		const dirGrid = path.join(logDir, "grid.jsonl");
		if (fs.existsSync(dirGrid)) {
			return {
				grid: dirGrid,
				cast: path.join(logDir, "cast"),
				ai: path.join(logDir, "ai.jsonl"),
				raw: path.join(logDir, "raw.log"),
			};
		}
	}

	// 2. Scan base logs dir for {date}_{windowId}/ pattern (structured_log_dir not set in registry)
	if (windowId) {
		try {
			const entries = fs.readdirSync(baseLogDir, { withFileTypes: true });
			const sessionDir = entries.find(
				(e) => e.isDirectory() && e.name.includes(windowId),
			);
			if (sessionDir) {
				const dirPath = path.join(baseLogDir, sessionDir.name);
				const dirGrid = path.join(dirPath, "grid.jsonl");
				if (fs.existsSync(dirGrid)) {
					return {
						grid: dirGrid,
						cast: path.join(dirPath, "cast"),
						ai: path.join(dirPath, "ai.jsonl"),
						raw: path.join(dirPath, "raw.log"),
					};
				}
			}
		} catch {
			// Directory missing or unreadable
		}
	}

	// 3. Legacy flat file: try exact match with session name
	if (sessionName) {
		const directGrid = path.join(baseLogDir, `${sessionName}.grid.jsonl`);
		if (fs.existsSync(directGrid)) {
			return {
				grid: directGrid,
				cast: path.join(baseLogDir, `${sessionName}.cast`),
				ai: path.join(baseLogDir, `${sessionName}.ai.jsonl`),
				raw: path.join(baseLogDir, `${sessionName}.log`),
			};
		}
	}

	// 4. Legacy flat file: scan for PID-prefixed files matching window_id
	if (windowId) {
		try {
			const files = fs.readdirSync(baseLogDir);
			const gridFile = files.find(
				(f) => f.endsWith(".grid.jsonl") && f.includes(windowId),
			);
			if (gridFile) {
				const base = gridFile.slice(0, -".grid.jsonl".length);
				return {
					grid: path.join(baseLogDir, gridFile),
					cast: path.join(baseLogDir, `${base}.cast`),
					ai: path.join(baseLogDir, `${base}.ai.jsonl`),
					raw: path.join(baseLogDir, `immorterm-${windowId}.log`),
				};
			}
		} catch {
			// Directory missing or unreadable
		}
	}

	// 5. Last resort: construct path from whatever we have (new dir format preferred)
	if (entry.structured_log_dir) {
		return {
			grid: path.join(logDir, "grid.jsonl"),
			cast: path.join(logDir, "cast"),
			ai: path.join(logDir, "ai.jsonl"),
			raw: path.join(logDir, "raw.log"),
		};
	}
	const fallbackName =
		sessionName || (windowId ? `immorterm-${windowId}` : "unknown");
	return {
		grid: path.join(baseLogDir, `${fallbackName}.grid.jsonl`),
		cast: path.join(baseLogDir, `${fallbackName}.cast`),
		ai: path.join(baseLogDir, `${fallbackName}.ai.jsonl`),
		raw: path.join(baseLogDir, `${fallbackName}.log`),
	};
}

// ---------------------------------------------------------------------------
// Enrichment
// ---------------------------------------------------------------------------

export function enrichSession(entry: RegistryEntry): EnrichedSession {
	const logPaths = resolveLogPaths(entry);

	const gridInfo = statLogFile(logPaths.grid);
	const castInfo = statLogFile(logPaths.cast);
	const aiInfo = statLogFile(logPaths.ai);
	const rawInfo = statLogFile(logPaths.raw);

	// Count snapshots/events for small files (skip for large files to avoid blocking)
	if (gridInfo.exists && gridInfo.size < 5 * 1024 * 1024) {
		gridInfo.snapshots = countLines(logPaths.grid);
	}
	if (aiInfo.exists && aiInfo.size < 2 * 1024 * 1024) {
		aiInfo.events = countLines(logPaths.ai);
	}

	return {
		id: deriveSessionId(entry),
		pid: entry.pid,
		displayName: entry.display_name,
		title: entry.title,
		projectDir: entry.project_dir,
		shell: entry.shell,
		createdAt: entry.created_at,
		sessionType: entry.session_type ?? "regular",
		status: isProcessAlive(entry.pid) ? "alive" : "dead",
		claudeSessionId: entry.claude_session_id,
		claudeStats: entry.claude_stats,
		theme: entry.theme,
		logs: {
			grid: gridInfo,
			cast: castInfo,
			ai: aiInfo,
			raw: rawInfo,
		},
	};
}

export function enrichAllSessions(projectFilter?: string): EnrichedSession[] {
	const registry = readRegistry();
	let sessions = registry.sessions;

	if (projectFilter) {
		sessions = sessions.filter((s) => s.project_dir === projectFilter);
	}

	return sessions.map(enrichSession);
}

// ---------------------------------------------------------------------------
// Resolve log file path for a session by ID
// ---------------------------------------------------------------------------

export function resolveSessionLogPath(
	sessionId: string,
	logType: "grid" | "cast" | "ai" | "raw",
): string | null {
	const registry = readRegistry();
	const entry = registry.sessions.find(
		(s) => s.window_id === sessionId || s.name === sessionId,
	);
	if (!entry) return null;

	const paths = resolveLogPaths(entry);
	return paths[logType];
}

// ---------------------------------------------------------------------------
// Archived session scanning
// ---------------------------------------------------------------------------

/** Read and enrich sessions from the archive directory of a project. */
export function enrichArchivedSessions(projectDir: string): EnrichedSession[] {
	const archiveDir = path.join(
		projectDir,
		".immorterm",
		"terminals",
		"logs",
		"archive",
	);
	if (!fs.existsSync(archiveDir)) return [];

	const results: EnrichedSession[] = [];
	try {
		const entries = fs.readdirSync(archiveDir, { withFileTypes: true });
		for (const entry of entries) {
			if (!entry.isDirectory()) continue;
			const sessionDir = path.join(archiveDir, entry.name);
			const sessionJsonPath = path.join(sessionDir, "session.json");

			let meta: Record<string, unknown> = {};
			try {
				const raw = fs.readFileSync(sessionJsonPath, "utf-8");
				meta = JSON.parse(raw);
			} catch {
				// No session.json — use directory name for minimal metadata
			}

			const gridPath = path.join(sessionDir, "grid.jsonl");
			const castPath = path.join(sessionDir, "cast");
			const aiPath = path.join(sessionDir, "ai.jsonl");
			const rawPath = path.join(sessionDir, "raw.log");

			const gridInfo = statLogFile(gridPath);
			const castInfo = statLogFile(castPath);
			const aiInfo = statLogFile(aiPath);
			const rawInfo = statLogFile(rawPath);

			if (gridInfo.exists && gridInfo.size < 5 * 1024 * 1024) {
				gridInfo.snapshots = countLines(gridPath);
			}
			if (aiInfo.exists && aiInfo.size < 2 * 1024 * 1024) {
				aiInfo.events = countLines(aiPath);
			}

			const windowId = (meta.window_id as string) || entry.name;
			results.push({
				id: `archived:${entry.name}`,
				pid: (meta.pid as number) || 0,
				displayName: (meta.display_name as string) || entry.name,
				title: (meta.title as string) || "",
				projectDir: (meta.project_dir as string) || projectDir,
				shell: (meta.shell as string) || "",
				createdAt: (meta.created_at as number) || 0,
				sessionType: (meta.session_type as string) || "regular",
				status: "dead",
				claudeSessionId: (meta.claude_session_id as string) || null,
				claudeStats: meta.claude_stats as RegistryEntry["claude_stats"],
				theme: (meta.theme as string) || null,
				isArchived: true,
				archivedAt: (meta.archived_at as number) || undefined,
				logs: {
					grid: gridInfo,
					cast: castInfo,
					ai: aiInfo,
					raw: rawInfo,
				},
			});
		}
	} catch {
		// Archive directory not readable
	}

	// Sort by createdAt descending (newest first)
	results.sort((a, b) => b.createdAt - a.createdAt);
	return results;
}

/** Resolve log file path for an archived session by archive directory name. */
export function resolveArchivedSessionLogPath(
	projectDir: string,
	archiveDirName: string,
	logType: "grid" | "cast" | "ai" | "raw",
): string | null {
	const archiveDir = path.join(
		projectDir,
		".immorterm",
		"terminals",
		"logs",
		"archive",
		archiveDirName,
	);
	const fileMap = {
		grid: "grid.jsonl",
		cast: "cast",
		ai: "ai.jsonl",
		raw: "raw.log",
	} as const;
	const filePath = path.join(archiveDir, fileMap[logType]);
	return fs.existsSync(filePath) ? filePath : null;
}

export function findRegistryEntry(sessionId: string): RegistryEntry | null {
	const registry = readRegistry();
	return (
		registry.sessions.find(
			(s) => s.window_id === sessionId || s.name === sessionId,
		) ?? null
	);
}
