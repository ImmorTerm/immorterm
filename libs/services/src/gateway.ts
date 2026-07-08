/**
 * MCP Gateway Manager — IDE-Independent
 *
 * Manages the lifecycle of the immorterm-mcp-gateway process.
 * No VS Code dependency — config reads/writes are caller's responsibility.
 */

import { execFile, fork, type ChildProcess } from "node:child_process";
import * as fs from "node:fs";
import * as http from "node:http";
import * as os from "node:os";
import * as path from "node:path";
import type { GatewayState, Logger } from "./types.js";

// ── Constants ──────────────────────────────────────────────────────

/** Default gateway port */
export const GATEWAY_PORT = 9100;

/** State directory for gateway runtime files */
export const GATEWAY_STATE_DIR = path.join(os.homedir(), ".immorterm", "mcp-gateway");

/** State file path */
export const GATEWAY_STATE_FILE = path.join(GATEWAY_STATE_DIR, "state.json");

/** Gateway health endpoint */
export function getHealthUrl(port: number = GATEWAY_PORT): string {
	return `http://localhost:${port}/health`;
}

// ── Health ─────────────────────────────────────────────────────────

/** Check gateway health via HTTP */
export async function checkGatewayHealth(port: number = GATEWAY_PORT): Promise<GatewayState> {
	const state: GatewayState = {
		running: false,
		healthy: false,
		port,
		pid: readPidFromState(),
	};

	const healthy = await new Promise<boolean>((resolve) => {
		const req = http.get(getHealthUrl(port), { timeout: 3000 }, (res) => {
			let data = "";
			res.on("data", (chunk) => {
				data += chunk;
			});
			res.on("end", () => {
				if (res.statusCode === 200) {
					try {
						const health = JSON.parse(data);
						state.serverCount = health.servers?.length ?? 0;
						state.activeChildren = health.totalChildren ?? 0;
						state.memoryMB = health.memoryMB ?? 0;
					} catch {}
					resolve(true);
				} else {
					resolve(false);
				}
			});
		});
		req.on("error", () => resolve(false));
		req.on("timeout", () => {
			req.destroy();
			resolve(false);
		});
	});

	state.running = healthy;
	state.healthy = healthy;
	return state;
}

// ── Lifecycle ──────────────────────────────────────────────────────

/** Start the gateway process (detached, survives parent exit) */
export async function startGateway(
	port: number = GATEWAY_PORT,
	log?: Logger,
): Promise<GatewayState> {
	log?.info("Starting MCP gateway...");

	// Check if already running
	const existing = await checkGatewayHealth(port);
	if (existing.healthy) {
		log?.info("Already running and healthy");
		return existing;
	}

	fs.mkdirSync(GATEWAY_STATE_DIR, { recursive: true });

	const gatewayPath = findGatewayBinary();
	if (!gatewayPath) {
		const errMsg = "Gateway binary not found. Run: npm install -g immorterm-mcp-gateway";
		log?.error(errMsg);
		return { running: false, healthy: false, port, lastError: errMsg };
	}

	const state: GatewayState = { running: false, healthy: false, port };

	try {
		const child: ChildProcess = fork(
			gatewayPath,
			["start", "--foreground", "--port", String(port)],
			{
				detached: true,
				stdio: ["ignore", "pipe", "pipe", "ipc"],
				env: { ...process.env },
			},
		);

		const started = await new Promise<boolean>((resolve) => {
			const timeout = setTimeout(() => {
				state.lastError = "Timed out waiting for gateway to start (30s)";
				resolve(false);
			}, 30_000);

			child.on("message", (msg: any) => {
				if (msg?.type === "started") {
					clearTimeout(timeout);
					state.pid = msg.pid;
					state.port = msg.port;
					resolve(true);
				}
			});

			child.on("error", (err) => {
				clearTimeout(timeout);
				state.lastError = err.message;
				resolve(false);
			});

			child.on("exit", (code) => {
				clearTimeout(timeout);
				if (code !== 0) {
					state.lastError = `Gateway exited with code ${code}`;
				} else {
					state.lastError = "Gateway process exited unexpectedly";
				}
				resolve(false);
			});
		});

		if (started) {
			child.unref();
			child.disconnect();

			await new Promise((r) => setTimeout(r, 1000));
			const healthResult = await checkGatewayHealth(state.port);

			state.running = true;
			state.healthy = healthResult.healthy;
			state.serverCount = healthResult.serverCount;
			state.activeChildren = healthResult.activeChildren;
			state.memoryMB = healthResult.memoryMB;
			log?.info(`Started (PID ${state.pid}, port ${state.port})`);
		} else {
			log?.error(`Failed to start: ${state.lastError}`);
		}
	} catch (err) {
		state.lastError = err instanceof Error ? err.message : String(err);
		log?.error(`Start error: ${state.lastError}`);
	}

	return state;
}

/** Stop the gateway process gracefully */
export async function stopGateway(port: number = GATEWAY_PORT, log?: Logger): Promise<void> {
	log?.info("Stopping MCP gateway...");

	const pid = readPidFromState();
	if (!pid) {
		log?.info("No PID found, nothing to stop");
		return;
	}

	try {
		process.kill(pid, "SIGTERM");
		log?.info(`Sent SIGTERM to PID ${pid}`);

		await new Promise<void>((resolve) => {
			let checks = 0;
			const interval = setInterval(() => {
				try {
					process.kill(pid, 0);
					checks++;
					if (checks > 10) {
						clearInterval(interval);
						process.kill(pid, "SIGKILL");
						resolve();
					}
				} catch {
					clearInterval(interval);
					resolve();
				}
			}, 500);
		});
	} catch (err: any) {
		if (err.code !== "ESRCH") {
			log?.error(`Stop error: ${err.message}`);
		}
	}
}

// ── Helpers ────────────────────────────────────────────────────────

/** Read PID from state.json file */
function readPidFromState(): number | undefined {
	try {
		if (fs.existsSync(GATEWAY_STATE_FILE)) {
			const data = JSON.parse(fs.readFileSync(GATEWAY_STATE_FILE, "utf-8"));
			return data.pid;
		}
	} catch {}
	return undefined;
}

/** Find the gateway binary — check local, global npm, mono-repo */
function findGatewayBinary(): string | null {
	// 1. Check common global install location
	try {
		const { execFileSync } = await_import_sync();
		const globalDir = execFileSync("npm", ["root", "-g"], {
			encoding: "utf-8",
			timeout: 5000,
		}).trim();
		const npmGlobalPath = path.join(globalDir, "immorterm-mcp-gateway", "dist", "index.js");
		if (fs.existsSync(npmGlobalPath)) return npmGlobalPath;
	} catch {}

	// 2. Check in the mono-repo during development
	const monoRepoPath = path.join(
		os.homedir(),
		"Development",
		"immorterm",
		"services",
		"mcp-gateway",
		"dist",
		"index.js",
	);
	if (fs.existsSync(monoRepoPath)) return monoRepoPath;

	// 3. Check relative to this file (if installed as dependency)
	const localPath = path.join(__dirname, "..", "..", "services", "mcp-gateway", "dist", "index.js");
	if (fs.existsSync(localPath)) return localPath;

	return null;
}

/** Helper to get execFileSync without top-level require */
function await_import_sync() {
	// eslint-disable-next-line @typescript-eslint/no-require-imports
	return require("node:child_process") as typeof import("node:child_process");
}
