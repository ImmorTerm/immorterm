/**
 * immorterm memory [up|down|status|install] — Manage native memory service
 */

import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import * as fs from "node:fs";
import * as http from "node:http";
import {
	MEMORY_PID_FILE,
	MEMORY_LOG_FILE,
	findBinary,
	getMemoryPort,
	checkMemoryHealth,
	installMemoryBinary,
	preflightMemoryBinary,
	isMemoryFirstBoot,
	spawnMemoryDaemon,
	tailMemoryLog,
	waitForMemory,
} from "@immorterm/services";
import { ensureProjectMemoryHooks } from "../lib/project-hooks.js";

function httpGet(url: string): Promise<string> {
	return new Promise((resolve, reject) => {
		http.get(url, { timeout: 3000 }, (res) => {
			let data = "";
			res.on("data", (chunk: Buffer) => data += chunk);
			res.on("end", () => resolve(data));
		}).on("error", reject);
	});
}

function sleep(ms: number): Promise<void> {
	return new Promise((r) => setTimeout(r, ms));
}

function memoryUrl(): string {
	return `http://127.0.0.1:${getMemoryPort()}/health`;
}

async function memoryUp(): Promise<void> {
	if (await checkMemoryHealth()) {
		consola.success(`Memory service is already ${pc.green("running")} and healthy.`);
		consola.info(`Memory service: ${pc.cyan(memoryUrl())}`);
		return;
	}

	const binary = findBinary();
	if (!binary) {
		consola.error("Memory binary not found.");
		consola.info(`Install with: ${pc.cyan("immorterm memory install")}`);
		process.exit(1);
	}

	// Preflight: a binary built against a newer glibc dies in the dynamic
	// loader — spawning it detached would fail silently.
	const loaderError = preflightMemoryBinary(binary);
	if (loaderError) {
		consola.error("Memory binary cannot run on this system:");
		consola.error(pc.dim(loaderError));
		consola.info(
			"The downloaded binary was built against newer system libraries (glibc) than this OS provides. " +
			"This is a packaging issue on our side, not something wrong with your system.",
		);
		consola.info(`Reinstalling after the next release may help: ${pc.cyan("immorterm memory install")}`);
		process.exit(1);
	}

	consola.start(`Starting ${binary}...`);

	const firstBoot = isMemoryFirstBoot();
	if (firstBoot) {
		consola.info("First start downloads ~150MB of models — this can take a few minutes.");
	}
	spawnMemoryDaemon(binary);

	const timeoutMs = firstBoot ? 180_000 : 10_000;
	const healthy = await waitForMemory(timeoutMs, 500, (elapsedSec) => {
		consola.info(pc.dim(`Still waiting for memory service... (${elapsedSec}s elapsed)`));
	});
	if (healthy) {
		consola.success(`Memory service ${pc.green("started")} successfully.`);
		consola.info(`Memory service: ${pc.cyan(memoryUrl())}`);
		return;
	}

	consola.error(`Failed to start within ${timeoutMs / 1000} seconds.`);
	consola.info(`Daemon log: ${MEMORY_LOG_FILE}`);
	const tail = tailMemoryLog(10);
	if (tail) {
		consola.info(pc.dim(tail));
	}
	process.exit(1);
}

async function memoryDown(): Promise<void> {
	try {
		const pidStr = fs.readFileSync(MEMORY_PID_FILE, "utf-8").trim();
		const pid = parseInt(pidStr, 10);

		process.kill(pid, "SIGTERM");
		consola.start(`Sent SIGTERM to PID ${pid}`);

		for (let i = 0; i < 10; i++) {
			await sleep(500);
			try {
				process.kill(pid, 0);
			} catch {
				consola.success(`Memory service ${pc.dim("stopped")}.`);
				return;
			}
		}
		consola.warn("Process did not exit within 5 seconds.");
	} catch (e: any) {
		if (e.code === "ENOENT") {
			consola.info("No PID file found. Service may not be running.");
		} else {
			consola.error(`Error: ${e.message}`);
		}
	}
}

async function memoryStatus(): Promise<void> {
	const healthy = await checkMemoryHealth();

	let pid: number | null = null;
	try {
		pid = parseInt(fs.readFileSync(MEMORY_PID_FILE, "utf-8").trim(), 10);
		try { process.kill(pid, 0); } catch { pid = null; }
	} catch { /* no pid file */ }

	const binary = findBinary();
	const binaryIcon = binary ? pc.green("found") : pc.red("NOT FOUND");
	const runIcon = pid ? pc.green(`Yes (PID ${pid})`) : pc.red("No");
	const healthIcon = healthy ? pc.green("Yes") : pc.red("No");

	consola.info(pc.bold("Memory Service Status"));
	consola.info(`  Binary:  ${binary ?? "NOT FOUND"} ${pc.dim(`[${binaryIcon}]`)}`);
	consola.info(`  Running: ${runIcon}`);
	consola.info(`  Healthy: ${healthIcon}`);
	consola.info(`  URL:     ${memoryUrl()}`);
	consola.info(`  Log:     ${MEMORY_LOG_FILE}`);

	if (healthy) {
		try {
			const body = await httpGet(memoryUrl());
			const data = JSON.parse(body);
			if (data.uptime) consola.info(`  Uptime:  ${data.uptime}`);
			if (data.rss_mb) consola.info(`  RSS:     ${data.rss_mb} MB`);
		} catch { /* basic health only */ }
	}
}

async function memoryInstall(): Promise<void> {
	const existing = findBinary();
	if (existing) {
		consola.success(`Memory binary already installed: ${pc.dim(existing)}`);
	} else {
		try {
			const installed = await installMemoryBinary({
				info: (msg) => consola.info(msg),
				warn: (msg) => consola.warn(msg),
				error: (msg) => consola.error(msg),
			});
			consola.success(`Memory binary installed: ${pc.dim(installed)}`);
			consola.info(`Start it with: ${pc.cyan("immorterm memory up")}`);
		} catch (e: any) {
			consola.error(`Install failed: ${e.message}`);
			consola.info(`Manual install: ${pc.cyan("https://github.com/ImmorTerm/immorterm/releases")}`);
			process.exit(1);
		}
	}

	// Final step: ensure hooks exist for the current project. Covers the
	// init-before-binary ordering (init installs hooks only when memory is
	// enabled) and makes `memory install` a complete "make memory work here".
	ensureProjectMemoryHooks(process.cwd());
}

export const memoryCommand = defineCommand({
	meta: {
		name: "memory",
		description: "Manage native memory service (up, down, status, install)",
	},
	args: {
		action: {
			type: "positional",
			description: "Action: up, down, status, install (default: status)",
			required: false,
		},
	},
	async run({ args }) {
		const action = (args.action as string | undefined) || "status";

		switch (action) {
			case "up":
				await memoryUp();
				break;
			case "down":
				await memoryDown();
				break;
			case "status":
				await memoryStatus();
				break;
			case "install":
				await memoryInstall();
				break;
			default:
				consola.error(`Unknown action: ${action}`);
				consola.info(`Usage: ${pc.cyan("npx immorterm memory [up|down|status|install]")}`);
		}
	},
});
