/**
 * Hook health checking — filesystem-based diagnostics for .claude/hooks/ scripts.
 *
 * Checks:
 * 1. Hook scripts installed in .claude/hooks/
 * 2. Recent log activity (any log modified within 1 hour)
 * 3. Error log files (non-empty = hook is erroring)
 * 4. Digest daemon PID file + process alive
 */

import * as fs from "node:fs";
import * as path from "node:path";
import * as os from "node:os";

export interface HookHealthResult {
	/** Whether any immorterm-*.sh hooks exist in .claude/hooks/ */
	installed: boolean;
	/** Number of immorterm-*.sh files found */
	hookCount: number;
	/** Whether any hook log was modified within 1 hour */
	recentActivity: boolean;
	/** Seconds since the most recent log entry (undefined if no logs) */
	lastActivityAge?: number;
	/** Number of hooks that have non-empty error logs */
	errorCount: number;
	/** Filenames of hooks with errors (e.g. ["code-capture", "task-persist"]) */
	errors: string[];
	/** Whether the digest daemon PID file exists and process is alive */
	daemonRunning: boolean;
	/** Seconds since last digest daemon log entry (undefined if no daemon log) */
	daemonLastCycle?: number;
}

/**
 * Check the health of ImmorTerm hook scripts.
 *
 * @param projectRoot - The project root directory (contains .claude/hooks/)
 * @returns Hook health diagnostics
 */
export async function checkHookHealth(
	projectRoot: string,
): Promise<HookHealthResult> {
	const result: HookHealthResult = {
		installed: false,
		hookCount: 0,
		recentActivity: false,
		errorCount: 0,
		errors: [],
		daemonRunning: false,
	};

	// Hooks live in .immorterm/hooks/ (installer target — extension + CLI) or
	// .claude/hooks/ (legacy / hand-managed). Count the union of both.
	const hookDirs = [
		path.join(projectRoot, ".immorterm", "hooks"),
		path.join(projectRoot, ".claude", "hooks"),
	];
	const logsDir = path.join(
		projectRoot,
		".immorterm",
		"terminals",
		"hooks",
		"logs",
	);
	const errorsDir = path.join(
		projectRoot,
		".immorterm",
		"terminals",
		"hooks",
		"errors",
	);

	// 1. Count installed hooks (union of both dirs, deduped by filename)
	const hookNames = new Set<string>();
	for (const dir of hookDirs) {
		try {
			for (const f of fs.readdirSync(dir)) {
				if (f.startsWith("immorterm-") && f.endsWith(".sh")) {
					hookNames.add(f);
				}
			}
		} catch {
			// Directory doesn't exist or not readable
		}
	}
	result.hookCount = hookNames.size;
	result.installed = hookNames.size > 0;

	// 2. Check recent log activity
	const now = Date.now();
	const ONE_HOUR_MS = 3600 * 1000;

	try {
		const logFiles = fs.readdirSync(logsDir).filter((f) => f.endsWith(".log"));
		let newestMtime = 0;

		for (const logFile of logFiles) {
			try {
				const stat = fs.statSync(path.join(logsDir, logFile));
				if (stat.mtimeMs > newestMtime) {
					newestMtime = stat.mtimeMs;
				}
			} catch {
				// Skip unreadable files
			}
		}

		if (newestMtime > 0) {
			result.lastActivityAge = Math.floor((now - newestMtime) / 1000);
			result.recentActivity = now - newestMtime < ONE_HOUR_MS;
		}
	} catch {
		// Logs directory doesn't exist
	}

	// 3. Check error logs (non-empty = hook is erroring)
	try {
		const errorFiles = fs
			.readdirSync(errorsDir)
			.filter((f) => f.endsWith(".log"));

		for (const errorFile of errorFiles) {
			try {
				const stat = fs.statSync(path.join(errorsDir, errorFile));
				if (stat.size > 0) {
					// Extract hook name from filename (e.g. "code-capture.log" -> "code-capture")
					const hookName = errorFile.replace(/\.log$/, "");
					result.errors.push(hookName);
				}
			} catch {
				// Skip unreadable files
			}
		}
		result.errorCount = result.errors.length;
	} catch {
		// Errors directory doesn't exist
	}

	// 4. Check digest daemon
	const immortermDir = path.join(os.homedir(), ".immorterm");

	// Find project ID from .immorterm/config.json
	const projectConfigPath = path.join(
		projectRoot,
		".immorterm",
		"config.json",
	);
	let projectId = "";
	try {
		const configData = JSON.parse(fs.readFileSync(projectConfigPath, "utf-8"));
		projectId = configData.projectId ?? "";
	} catch {
		// No project config — can't check daemon
	}

	if (projectId) {
		const pidFile = path.join(
			immortermDir,
			`digest-daemon-${projectId}.pid`,
		);
		try {
			const pidStr = fs.readFileSync(pidFile, "utf-8").trim();
			const pid = Number.parseInt(pidStr, 10);
			if (!Number.isNaN(pid)) {
				// Check if process is alive (signal 0 doesn't kill, just checks)
				try {
					process.kill(pid, 0);
					result.daemonRunning = true;
				} catch {
					// Process not running — stale PID file
				}
			}
		} catch {
			// PID file doesn't exist
		}

		// Check daemon log freshness
		const daemonLog = path.join(immortermDir, "digest-daemon.log");
		try {
			const stat = fs.statSync(daemonLog);
			result.daemonLastCycle = Math.floor((now - stat.mtimeMs) / 1000);
		} catch {
			// No daemon log
		}
	}

	return result;
}
