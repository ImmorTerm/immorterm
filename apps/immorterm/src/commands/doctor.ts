/**
 * immorterm doctor — Full diagnostic
 */

import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import * as fs from "node:fs";
import * as path from "node:path";
import { readGlobalConfig, IMMORTERM_GLOBAL_DIR } from "@immorterm/config";
import {
	refreshMemoryState,
	findBinary,
	checkGatewayHealth,
	checkHookHealth,
	preflightMemoryBinary,
	GATEWAY_PORT,
} from "@immorterm/services";

interface CheckResult {
	name: string;
	status: "pass" | "warn" | "fail";
	detail: string;
	/** One-line remediation command, printed under failed checks */
	hint?: string;
}

/** Run all diagnostic checks and return results (reusable from InteractiveApp) */
export async function runDoctorChecks(): Promise<CheckResult[]> {
	const checks: CheckResult[] = [];

	// 1. Global config
	const configPath = path.join(IMMORTERM_GLOBAL_DIR, "config.json");
	const configExists = fs.existsSync(configPath);
	checks.push({
		name: "Config",
		status: configExists ? "pass" : "warn",
		detail: configExists ? configPath : "Not initialized. Run: immorterm init",
	});

	// 2. Memory binary — exec it, don't just stat: a binary built against a
	// newer glibc exists on disk but dies in the dynamic loader.
	const binary = findBinary();
	if (!binary) {
		checks.push({
			name: "Memory Binary",
			status: "warn",
			detail: "Not found",
			hint: "npx immorterm memory install",
		});
	} else {
		const loaderError = preflightMemoryBinary(binary);
		checks.push(
			loaderError
				? {
					name: "Memory Binary",
					status: "fail",
					detail: `Found at ${binary} but cannot run: ${loaderError.split("\n")[0]}`,
					hint: "OS glibc too old — reinstall after next release: npx immorterm memory install",
				}
				: { name: "Memory Binary", status: "pass", detail: `Found at ${binary}` },
		);
	}

	// 3. Memory service
	const mem = await refreshMemoryState();
	checks.push({
		name: "Memory Service",
		status: mem.apiHealthy ? "pass" : mem.running ? "warn" : "fail",
		detail: mem.apiHealthy
			? `API healthy, MCP: ${mem.mcpHealthy ? "healthy" : "unhealthy"}`
			: mem.running
				? "Running but API unhealthy"
				: "Not running",
		hint: mem.apiHealthy ? undefined : "npx immorterm memory up",
	});

	// 4. MCP Gateway
	const config = readGlobalConfig();
	const gw = await checkGatewayHealth(GATEWAY_PORT);
	if (config.defaults.services.mcpGateway.enabled) {
		checks.push({
			name: "MCP Gateway",
			status: gw.healthy ? "pass" : "fail",
			detail: gw.healthy
				? `Healthy — ${gw.serverCount} servers, ${gw.activeChildren} active, ${gw.memoryMB}MB`
				: gw.lastError ?? "Not running",
			hint: gw.healthy ? undefined : "npx immorterm start",
		});
	} else {
		checks.push({ name: "MCP Gateway", status: "pass", detail: "Disabled" });
	}

	// 5. Disk usage
	try {
		const { execFileSync } = await import("node:child_process");
		const du = execFileSync("du", ["-sh", IMMORTERM_GLOBAL_DIR], {
			encoding: "utf-8",
			timeout: 5000,
		}).trim();
		const size = du.split("\t")[0] ?? "unknown";
		checks.push({ name: "Disk Usage", status: "pass", detail: `${size} in ~/.immorterm` });
	} catch {
		checks.push({ name: "Disk Usage", status: "warn", detail: "Could not check" });
	}

	// 6. Hook Health
	const hookHealth = await checkHookHealth(process.cwd());
	checks.push({
		name: "Hooks",
		status: hookHealth.errorCount > 0
			? "warn"
			: hookHealth.installed && hookHealth.recentActivity
				? "pass"
				: hookHealth.installed
					? "warn"
					: "fail",
		detail: hookHealth.installed
			? `${hookHealth.hookCount} installed, ${hookHealth.recentActivity ? "active" : "stale"}${hookHealth.errorCount > 0 ? `, ${hookHealth.errorCount} with errors` : ""}${hookHealth.daemonRunning ? ", daemon running" : ""}`
			: "Not installed. Memory hooks missing.",
		hint: hookHealth.installed ? undefined : "npx immorterm init",
	});

	// 7. License
	const lic = config.license;
	checks.push({
		name: "License",
		status: "pass",
		detail: lic.status === "active"
			? `Pro (${lic.customerEmail ?? ""})`
			: "Free tier",
	});

	return checks;
}

export const doctorCommand = defineCommand({
	meta: {
		name: "doctor",
		description: "Run full diagnostics (services, config, disk)",
	},
	async run() {
		consola.info(pc.bold("ImmorTerm Doctor"));
		consola.info(pc.dim("Running diagnostics..."));
		consola.info("");

		const checks = await runDoctorChecks();

		for (const check of checks) {
			const icon =
				check.status === "pass"
					? pc.green("✓")
					: check.status === "warn"
						? pc.yellow("!")
						: pc.red("✗");
			consola.info(`  ${icon} ${pc.bold(check.name)}: ${check.detail}`);
			if (check.hint && check.status !== "pass") {
				consola.info(`      ${pc.dim(`fix: ${check.hint}`)}`);
			}
		}

		const failures = checks.filter((c) => c.status === "fail");
		const warnings = checks.filter((c) => c.status === "warn");

		consola.info("");
		if (failures.length === 0 && warnings.length === 0) {
			consola.success("All checks passed!");
		} else if (failures.length === 0) {
			consola.info(`${pc.yellow(`${warnings.length} warning(s)`)}, no failures.`);
		} else {
			consola.error(`${pc.red(`${failures.length} failure(s)`)}, ${warnings.length} warning(s).`);
			process.exitCode = 1;
		}
	},
});
