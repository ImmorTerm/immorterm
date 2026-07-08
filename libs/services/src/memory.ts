/**
 * Native Memory Service Manager — IDE-Independent
 *
 * Manages the native Rust memory binary lifecycle:
 * - Binary detection
 * - Start/stop via PID file
 * - Health checks (REST + MCP)
 *
 * No Docker dependency. The native binary runs as a daemon process.
 */

import * as fs from "node:fs";
import * as http from "node:http";
import * as os from "node:os";
import * as path from "node:path";
import { execFileSync, spawn } from "node:child_process";
import type { Logger, MemoryState } from "./types.js";

// ── Constants ──────────────────────────────────────────────────────

/** State file written by the memory service on startup */
const STATE_FILE = path.join(os.homedir(), ".immorterm", "memory.state.json");

/** Default port if state file doesn't exist */
const DEFAULT_PORT = 8765;

/**
 * Read the memory service port from its state file.
 * Falls back to DEFAULT_PORT if state.json doesn't exist, is malformed, or PID is stale.
 */
export function getMemoryPort(): number {
	try {
		const content = fs.readFileSync(STATE_FILE, "utf-8");
		const state = JSON.parse(content);
		const pid = state.pid;
		const port = state.port;

		if (typeof port !== "number" || typeof pid !== "number") {
			return DEFAULT_PORT;
		}

		try {
			process.kill(pid, 0);
			return port;
		} catch {
			return DEFAULT_PORT;
		}
	} catch {
		return DEFAULT_PORT;
	}
}

/** Port used by the memory service (REST + MCP) — reads from state.json */
export const MEMORY_PORT = getMemoryPort();

/** Path to the native memory binary */
export const MEMORY_BINARY = path.join(os.homedir(), ".immorterm", "bin", "immorterm-memory");

/** PID file for the daemon process */
export const MEMORY_PID_FILE = path.join(os.homedir(), ".immorterm", "memory.pid");

/** Data directory (SQLite + vector index) */
export const MEMORY_DATA_DIR = path.join(os.homedir(), ".immorterm", "memory");

/** Daemon log file (same path used by .claude/hooks/lib/ensure-immorterm-memory.sh) */
export const MEMORY_LOG_FILE = path.join(os.homedir(), ".immorterm", "memory-daemon.log");

/** ONNX model directory (mirrors config.rs model_dir = data_dir/models) */
export const MEMORY_MODELS_DIR = path.join(os.homedir(), ".immorterm", "models");

// ── Binary Detection ───────────────────────────────────────────────

/**
 * Find the native memory binary. Probes, in order:
 * 1. ~/.immorterm/bin/immorterm-memory (canonical install location)
 * 2. Every directory on $PATH (covers `npm install -g immorterm-memory`)
 * 3. The Node runtime's bin dir (npm-global bin when $PATH is stripped)
 */
export function findBinary(): string | null {
	const binaryName = path.basename(MEMORY_BINARY);
	const candidates = [
		MEMORY_BINARY,
		...(process.env.PATH ?? "")
			.split(path.delimiter)
			.filter(Boolean)
			.map((dir) => path.join(dir, binaryName)),
		path.join(path.dirname(process.execPath), binaryName),
	];
	for (const candidate of candidates) {
		try {
			const stats = fs.statSync(candidate);
			if (stats.isFile() && (stats.mode & fs.constants.X_OK) !== 0) {
				return candidate;
			}
		} catch {}
	}
	return null;
}

// ── Binary Install ─────────────────────────────────────────────────

/** GitHub repo hosting memory releases (overridable for forks/testing) */
const GITHUB_REPO = process.env.IMMORTERM_GITHUB_REPO ?? "ImmorTerm/immorterm";

/** Release asset name for the current platform, or null if unsupported */
export function memoryAssetName(): string | null {
	const osName =
		process.platform === "darwin" ? "macos" : process.platform === "linux" ? "linux" : null;
	const arch = process.arch === "arm64" ? "aarch64" : process.arch === "x64" ? "x86_64" : null;
	return osName && arch ? `immorterm-memory-${osName}-${arch}.tar.gz` : null;
}

/**
 * Version stamp written next to the binary on install (the GitHub release
 * tag). The memory binary has no reliable --version; upgrade checks compare
 * this stamp against the latest release tag.
 */
export const MEMORY_VERSION_FILE = path.join(
	path.dirname(MEMORY_BINARY),
	".immorterm-memory.version",
);

/** Release tag recorded at install time, or null if never stamped */
export function getInstalledMemoryTag(): string | null {
	try {
		const tag = fs.readFileSync(MEMORY_VERSION_FILE, "utf-8").trim();
		return tag || null;
	} catch {
		return null;
	}
}

/**
 * Known-good memory release tag, pinned at CLI publish time. The API-free
 * fallback when api.github.com is rate-limited (anonymous 60/hr per IP —
 * corporate NATs and CI runners hit this constantly). The
 * releases/download/{tag}/{asset} endpoint is a plain CDN redirect with
 * no rate limit.
 */
export const PINNED_MEMORY_TAG = "memory-prod-2026-07-07.1";

/**
 * Newest GitHub release carrying this platform's memory asset
 * (the newest release doesn't always carry binaries).
 * Falls back to the pinned known-good tag when the API is unavailable
 * (rate limit / offline registry mirror). Returns null only when no
 * release has the asset.
 */
export async function findLatestMemoryRelease(): Promise<{ tag: string; url: string } | null> {
	const asset = memoryAssetName();
	if (!asset) {
		throw new Error(`Unsupported platform: ${process.platform}/${process.arch}`);
	}
	try {
		const res = await fetch(`https://api.github.com/repos/${GITHUB_REPO}/releases?per_page=30`, {
			headers: { Accept: "application/vnd.github+json" },
		});
		if (!res.ok) {
			throw new Error(`GitHub API error: HTTP ${res.status}`);
		}
		const releases = (await res.json()) as Array<{
			tag_name: string;
			assets: Array<{ name: string; browser_download_url: string }>;
		}>;
		// Releases come newest-first — first match is the newest binary
		for (const r of releases) {
			const match = r.assets.find((a) => a.name === asset);
			if (match) return { tag: r.tag_name, url: match.browser_download_url };
		}
		return null;
	} catch (err) {
		// API unavailable — fall back to the pinned release via the
		// rate-limit-free download endpoint.
		const url = `https://github.com/${GITHUB_REPO}/releases/download/${PINNED_MEMORY_TAG}/${asset}`;
		const head = await fetch(url, { method: "HEAD", redirect: "follow" }).catch(() => null);
		if (head?.ok) {
			return { tag: PINNED_MEMORY_TAG, url };
		}
		throw err;
	}
}

/**
 * Download and install the memory binary from GitHub releases into
 * ~/.immorterm/bin, stamping the release tag. Overwrites an existing
 * binary (callers must stop the daemon first). Throws on failure.
 */
export async function installMemoryBinary(log?: Logger): Promise<string> {
	const asset = memoryAssetName();

	log?.info("Finding latest memory release...");
	const release = await findLatestMemoryRelease();
	if (!release) {
		throw new Error(`No release asset named ${asset} found in ${GITHUB_REPO}`);
	}

	const binDir = path.dirname(MEMORY_BINARY);
	fs.mkdirSync(binDir, { recursive: true });
	const tmpTar = path.join(os.tmpdir(), `${asset}.${process.pid}`);

	log?.info(`Downloading ${asset} (${release.tag})...`);
	try {
		execFileSync("curl", ["-fsSL", release.url, "-o", tmpTar], {
			stdio: "ignore",
			timeout: 300000,
		});
		// ponytail: tar unlinks-then-writes, dodging ETXTBSY if a stale daemon lingers
		execFileSync("tar", ["xzf", tmpTar, "-C", binDir]);
	} finally {
		fs.rmSync(tmpTar, { force: true });
	}

	if (!fs.existsSync(MEMORY_BINARY)) {
		throw new Error(`Extraction succeeded but ${MEMORY_BINARY} is missing`);
	}
	fs.chmodSync(MEMORY_BINARY, 0o755);
	fs.writeFileSync(MEMORY_VERSION_FILE, `${release.tag}\n`);
	log?.info(`Installed ${MEMORY_BINARY} (${release.tag})`);
	return MEMORY_BINARY;
}

// ── Preflight & Daemon Helpers ─────────────────────────────────────

/** Dynamic-loader failure patterns: glibc too old, missing shared objects */
const LOADER_ERROR_RE = /GLIBC_|libmvec|cannot open shared object|not found \(required by/;

/**
 * Exec the binary with --help before daemonizing it. A binary built against a
 * newer glibc dies in the dynamic loader — `spawn` with detached/ignored stdio
 * would swallow that entirely. Returns the loader stderr on such failures,
 * null when the binary executes (any clean exit, even non-zero, is fine).
 */
export function preflightMemoryBinary(binary: string): string | null {
	try {
		execFileSync(binary, ["--help"], { stdio: ["ignore", "ignore", "pipe"], timeout: 10000 });
		return null;
	} catch (err) {
		const stderr = ((err as { stderr?: Buffer }).stderr?.toString() ?? "").trim();
		return LOADER_ERROR_RE.test(stderr) ? stderr : null;
	}
}

/** First boot = ONNX models not downloaded yet (~150MB pull on daemon start) */
export function isMemoryFirstBoot(): boolean {
	return !fs.existsSync(path.join(MEMORY_MODELS_DIR, "model.onnx"));
}

/** Last N lines of the daemon log, or null if unreadable/empty */
export function tailMemoryLog(lines = 10, file = MEMORY_LOG_FILE): string | null {
	try {
		const content = fs.readFileSync(file, "utf-8").trimEnd();
		return content ? content.split("\n").slice(-lines).join("\n") : null;
	} catch {
		return null;
	}
}

/** Spawn the memory daemon detached, with stdout/stderr appended to MEMORY_LOG_FILE */
export function spawnMemoryDaemon(binary: string): void {
	fs.mkdirSync(path.dirname(MEMORY_LOG_FILE), { recursive: true });
	const logFd = fs.openSync(MEMORY_LOG_FILE, "a");
	try {
		const child = spawn(binary, ["serve", "--daemon"], {
			detached: true,
			stdio: ["ignore", logFd, logFd],
		});
		child.unref();
	} finally {
		fs.closeSync(logFd);
	}
}

// ── Health Checks ──────────────────────────────────────────────────

/** Check memory REST API health */
export async function checkMemoryHealth(): Promise<boolean> {
	return new Promise((resolve) => {
		const req = http.get(
			`http://127.0.0.1:${MEMORY_PORT}/health`,
			{ timeout: 3000 },
			(res) => {
				resolve(res.statusCode === 200);
			},
		);
		req.on("error", () => resolve(false));
		req.on("timeout", () => {
			req.destroy();
			resolve(false);
		});
	});
}

/** Check MCP endpoint health via JSON-RPC initialize handshake */
export async function checkMcpEndpointHealth(): Promise<boolean> {
	return new Promise((resolve) => {
		const payload = JSON.stringify({
			jsonrpc: "2.0",
			method: "initialize",
			id: 1,
			params: {
				protocolVersion: "2025-03-26",
				capabilities: {},
				clientInfo: { name: "health-probe", version: "1.0" },
			},
		});

		const req = http.request(
			{
				hostname: "127.0.0.1",
				port: MEMORY_PORT,
				path: "/mcp/claude-code/health-probe",
				method: "POST",
				timeout: 5000,
				headers: {
					"Content-Type": "application/json",
					Accept: "application/json, text/event-stream",
					"Content-Length": Buffer.byteLength(payload),
				},
			},
			(res) => {
				let data = "";
				res.on("data", (chunk) => {
					data += chunk;
				});
				res.on("end", () => {
					if (!res.statusCode || res.statusCode < 200 || res.statusCode >= 300) {
						resolve(false);
						return;
					}
					try {
						const contentType = res.headers["content-type"] ?? "";
						let jsonStr: string;
						if (contentType.includes("text/event-stream")) {
							const dataLine = data.split("\n").find((l) => l.startsWith("data: "));
							jsonStr = dataLine ? dataLine.slice(6) : "";
						} else {
							jsonStr = data;
						}
						const result = JSON.parse(jsonStr);
						resolve(!!result?.result?.serverInfo);
					} catch {
						resolve(false);
					}
				});
			},
		);
		req.on("error", () => resolve(false));
		req.on("timeout", () => {
			req.destroy();
			resolve(false);
		});
		req.write(payload);
		req.end();
	});
}

/** Wait for memory API to become healthy, with an optional 10s progress heartbeat */
export async function waitForMemory(
	timeoutMs: number = 30000,
	intervalMs: number = 1000,
	onHeartbeat?: (elapsedSec: number) => void,
): Promise<boolean> {
	const startTime = Date.now();
	let lastBeat = startTime;
	while (Date.now() - startTime < timeoutMs) {
		if (await checkMemoryHealth()) return true;
		await new Promise((resolve) => setTimeout(resolve, intervalMs));
		if (onHeartbeat && Date.now() - lastBeat >= 10000) {
			onHeartbeat(Math.round((Date.now() - startTime) / 1000));
			lastBeat = Date.now();
		}
	}
	return false;
}

// ── Lifecycle ──────────────────────────────────────────────────────

/** Start the native memory service as a daemon */
export async function startMemory(log?: Logger): Promise<MemoryState> {
	const state: MemoryState = {
		running: false,
		apiHealthy: false,
		mcpHealthy: false,
	};

	// Check if already running
	if (await checkMemoryHealth()) {
		log?.info("Memory service already running and healthy");
		state.running = true;
		state.apiHealthy = true;
		state.mcpHealthy = await checkMcpEndpointHealth();
		return state;
	}

	// Check binary exists
	const binary = findBinary();
	if (!binary) {
		state.lastError = "Memory binary not found. Install via: npx immorterm memory install";
		log?.error(state.lastError);
		return state;
	}

	// Preflight: catch loader failures (old glibc) before daemonizing
	const loaderError = preflightMemoryBinary(binary);
	if (loaderError) {
		state.lastError =
			`Memory binary cannot run on this system (glibc too old or missing libraries):\n${loaderError}\n` +
			"Reinstalling after the next release may help: npx immorterm memory install";
		log?.error(state.lastError);
		return state;
	}

	// Start daemon
	log?.info("Starting native memory service...");
	const firstBoot = isMemoryFirstBoot();
	if (firstBoot) {
		log?.info("First start downloads ~150MB of models — this can take a few minutes");
	}
	spawnMemoryDaemon(binary);

	// Wait for health (first boot needs time for the model download)
	log?.info("Waiting for memory service to become healthy...");
	const timeoutMs = firstBoot ? 180000 : 10000;
	const healthy = await waitForMemory(timeoutMs, 1000, (elapsedSec) => {
		log?.info(`Still waiting for memory service... (${elapsedSec}s elapsed)`);
	});
	if (!healthy) {
		const tail = tailMemoryLog();
		state.lastError =
			`Memory service failed to start within ${timeoutMs / 1000}s. Log: ${MEMORY_LOG_FILE}` +
			(tail ? `\n${tail}` : "");
		log?.error(state.lastError);
		return state;
	}

	state.running = true;
	state.apiHealthy = true;
	state.mcpHealthy = await checkMcpEndpointHealth();
	state.startedAt = Date.now();
	log?.info(`Memory service started (MCP: ${state.mcpHealthy ? "OK" : "not ready"})`);
	return state;
}

/** Stop the native memory service */
export async function stopMemory(log?: Logger): Promise<void> {
	try {
		const pid = readPid();
		if (pid) {
			process.kill(pid, "SIGTERM");
			log?.info(`Sent SIGTERM to memory service (PID ${pid})`);
		} else {
			log?.info("No PID file found, nothing to stop");
		}
	} catch (err) {
		if ((err as NodeJS.ErrnoException).code === "ESRCH") {
			log?.info("Process already stopped");
			cleanupPidFile();
		} else {
			log?.error(`Failed to stop memory service: ${err}`);
		}
	}
}

/** Refresh memory service state by checking actual status */
export async function refreshMemoryState(): Promise<MemoryState> {
	const state: MemoryState = {
		running: false,
		apiHealthy: false,
		mcpHealthy: false,
	};

	state.apiHealthy = await checkMemoryHealth();
	state.running = state.apiHealthy || isPidAlive();
	state.mcpHealthy = state.apiHealthy ? await checkMcpEndpointHealth() : false;

	return state;
}

// ── PID File Helpers ───────────────────────────────────────────────

function readPid(): number | null {
	try {
		const content = fs.readFileSync(MEMORY_PID_FILE, "utf8").trim();
		const pid = parseInt(content, 10);
		return isNaN(pid) ? null : pid;
	} catch {
		return null;
	}
}

function isPidAlive(): boolean {
	const pid = readPid();
	if (!pid) return false;
	try {
		process.kill(pid, 0);
		return true;
	} catch {
		return false;
	}
}

function cleanupPidFile(): void {
	try {
		fs.unlinkSync(MEMORY_PID_FILE);
	} catch {}
}
