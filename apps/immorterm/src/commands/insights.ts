/**
 * immorterm insights — View proactive intelligence stats
 *
 * Shows engagement rates, signal effectiveness, failure patterns,
 * and memory category distribution from the ImmorTerm Memory service.
 */

import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import * as http from "node:http";
import * as fs from "node:fs";
import * as path from "node:path";

const DEFAULT_PORT = 8765;
const HOME = process.env.HOME || "/tmp";
const STATE_FILE = path.join(HOME, ".immorterm", "memory.state.json");

function getMemoryPort(): number {
	try {
		const content = fs.readFileSync(STATE_FILE, "utf-8");
		const state = JSON.parse(content);
		if (typeof state.port === "number" && typeof state.pid === "number") {
			try { process.kill(state.pid, 0); return state.port; } catch { /* stale */ }
		}
	} catch { /* no state file */ }
	return DEFAULT_PORT;
}

function httpGet(url: string): Promise<string> {
	return new Promise((resolve, reject) => {
		http.get(url, { timeout: 5000 }, (res) => {
			let data = "";
			res.on("data", (chunk: Buffer) => data += chunk);
			res.on("end", () => resolve(data));
		}).on("error", reject);
	});
}

/** Fetch insights data from the memory service API. */
export async function fetchInsights(): Promise<any> {
	const body = await httpGet(`http://127.0.0.1:${getMemoryPort()}/api/v1/stats/insights`);
	return JSON.parse(body);
}

export const insightsCommand = defineCommand({
	meta: {
		name: "insights",
		description: "View proactive intelligence stats",
	},
	async run() {
		let data: any;
		try {
			data = await fetchInsights();
		} catch {
			consola.error("Memory service unavailable. Start it with: " + pc.cyan("immorterm memory up"));
			process.exit(1);
		}

		if (data.error) {
			consola.error(data.error);
			process.exit(1);
		}

		const eng = data.engagement ?? {};
		const rate = eng.overall_rate != null ? eng.overall_rate.toFixed(1) : "0.0";

		console.log();
		console.log(pc.bold("  ImmorTerm Memory \u2014 Proactive Insights"));
		console.log(pc.dim("  " + "\u2500".repeat(38)));
		console.log(
			`  Suggestions: ${pc.bold(String(eng.total_shown ?? 0))}` +
			`   Acted On: ${pc.green(pc.bold(String(eng.total_acted_on ?? 0)))}` +
			`   Rate: ${pc.cyan(rate + "%")}` +
			`   Guardrails: ${pc.yellow(String(data.guardrails_active ?? 0))}`,
		);
		console.log();

		// Signal effectiveness
		const signals = eng.by_signal ?? {};
		const signalKeys = Object.keys(signals);
		if (signalKeys.length > 0) {
			console.log(pc.bold("  Signal Effectiveness"));
			for (const key of signalKeys) {
				const s = signals[key];
				const sRate = s.shown > 0 ? (s.acted_on / s.shown) * 100 : 0;
				const filled = Math.round(sRate / 6.25);
				const bar = pc.green("\u2588".repeat(filled)) + pc.dim("\u2591".repeat(16 - filled));
				const name = key.replace(/_/g, " ").padEnd(20);
				const color = sRate > 50 ? pc.green : sRate > 20 ? pc.yellow : pc.red;
				console.log(`  ${pc.dim(name)}${bar}  ${color(sRate.toFixed(1) + "%")}  ${pc.dim(`(${s.acted_on}/${s.shown})`)}`);
			}
			console.log();
		}

		// Failure patterns
		const patterns = data.failure_patterns ?? [];
		if (patterns.length > 0) {
			console.log(pc.bold("  Top Failure Patterns"));
			for (let i = 0; i < patterns.length; i++) {
				const p = patterns[i];
				console.log(`  ${pc.dim(String(i + 1) + ".")} ${pc.red(`[x${p.frequency}]`)} ${p.description}`);
			}
			console.log();
		}

		// Summary
		const ss = data.session_summary ?? {};
		console.log(
			pc.dim("  Sessions (7d): ") + (ss.total_7d ?? 0) +
			pc.dim("   Today: ") + (ss.active_today ?? 0) +
			pc.dim("   Lessons: ") + (data.lessons_count ?? 0),
		);

		// Categories
		const cats = data.memory_categories ?? [];
		if (cats.length > 0) {
			console.log();
			console.log(pc.bold("  Memory Categories"));
			for (const c of cats) {
				console.log(`  ${pc.dim(c.category.padEnd(20))} ${c.count}`);
			}
		}

		console.log();
	},
});
