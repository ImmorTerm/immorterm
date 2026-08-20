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

/**
 * Candidate npm global roots, without shelling out to `npm`.
 *
 * `npm root -g` was the only lookup here, and it is a bare `execFileSync("npm", …)`
 * that depends on PATH. A GUI-launched extension host does not inherit a login
 * shell's PATH, and version managers frequently shim `node` without shimming
 * `npm` — so that call threw, selection fell through to the hard-coded
 * development path below, and the gateway silently ran a STALE VENDORED COPY
 * for months. Nothing surfaced: the wrong gateway starts and answers happily.
 *
 * Global installs live at `<prefix>/lib/node_modules`, and the prefix is the
 * grandparent of the node binary — derivable from `process.execPath` under nvm,
 * volta, asdf, proto, Homebrew and system installs alike.
 */
function npmGlobalRoots(): string[] {
	const roots: string[] = [];
	const nodeDir = path.dirname(process.execPath); // …/bin
	const prefix = path.dirname(nodeDir); // …/
	roots.push(path.join(prefix, "lib", "node_modules"));
	// Homebrew keeps node's real prefix behind a symlinked shim on some setups.
	roots.push("/opt/homebrew/lib/node_modules", "/usr/local/lib/node_modules");
	const home = os.homedir();
	roots.push(
		path.join(home, ".proto", "tools", "node", "globals", "lib", "node_modules"),
		path.join(home, ".volta", "tools", "image", "packages"),
		path.join(home, ".npm-global", "lib", "node_modules"),
	);
	return roots;
}

/** Find the gateway binary — global install first, then a development checkout */
function findGatewayBinary(): string | null {
	// 1. Global npm install — the supported location, resolved WITHOUT `npm`.
	for (const root of npmGlobalRoots()) {
		const candidate = path.join(root, "immorterm-mcp-gateway", "dist", "index.js");
		if (fs.existsSync(candidate)) return candidate;
	}

	// 2. Relative to this file, when the gateway ships as a dependency.
	const localPath = path.join(__dirname, "..", "..", "services", "mcp-gateway", "dist", "index.js");
	if (fs.existsSync(localPath)) return localPath;

	// 3. A sibling checkout of the standalone repo, for development only.
	//    NOTE: the old code hard-coded ~/Development/immorterm/services/mcp-gateway
	//    — one developer's layout, and a path this repo no longer even contains
	//    since the gateway was extracted. It is gone deliberately: preferring a
	//    stale vendored copy over the installed one is worse than finding nothing.
	const siblingRepo = path.join(__dirname, "..", "..", "..", "immorterm-mcp-gateway", "dist", "index.js");
	if (fs.existsSync(siblingRepo)) return siblingRepo;

	return null;
}

/** Helper to get execFileSync without top-level require */
function await_import_sync() {
	// eslint-disable-next-line @typescript-eslint/no-require-imports
	return require("node:child_process") as typeof import("node:child_process");
}
