/**
 * immorterm vendors — manage which AI tools ImmorTerm hooks into, per project.
 *
 * `immorterm init` picks the initial set; this is how you change it afterwards
 * without re-running the whole wizard. Selection is PER-PROJECT (it decides
 * which config files land in the project root), so every subcommand operates on
 * the cwd's `.immorterm/config.json`.
 *
 *   immorterm vendors               # list, with detection + enabled state
 *   immorterm vendors add codex
 *   immorterm vendors remove claudeCode
 *   immorterm vendors set codex,cursor
 *
 * add/remove/set all re-run the installer, so removing a vendor deletes the
 * config files it owns (`.codex/hooks.json`, `.claude/settings.local.json`
 * entries, …) rather than just flipping a flag.
 */

import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import { defaultVendorsConfig, readProjectConfig } from "@immorterm/config";
import type { VendorId } from "@immorterm/config";
import { detectVendors, setProjectVendors } from "@immorterm/services";
import { ensureProjectMemoryHooks } from "../lib/project-hooks.js";

/** Vendor ids currently enabled for `projectRoot`, honouring the shipped default. */
function currentVendors(projectRoot: string): VendorId[] {
	const config = readProjectConfig(projectRoot);
	const map = config?.services?.vendors ?? defaultVendorsConfig();
	return (Object.keys(map) as VendorId[]).filter((id) => map[id]?.enabled);
}

/** Parse a comma-separated vendor list, reporting anything unrecognized. */
function parseIds(raw: string | undefined): VendorId[] | null {
	const known = new Set(Object.keys(defaultVendorsConfig()));
	const ids = (raw ?? "").split(",").map((s) => s.trim()).filter(Boolean);
	if (ids.length === 0) {
		consola.error("Name at least one AI tool. See `immorterm vendors` for the list.");
		return null;
	}
	const unknown = ids.filter((id) => !known.has(id));
	if (unknown.length > 0) {
		consola.error(`Unknown AI tool(s): ${unknown.join(", ")}`);
		consola.info(`Valid ids: ${pc.dim([...known].join(", "))}`);
		return null;
	}
	return ids as VendorId[];
}

/** Persist the selection and re-run the installer so files match it. */
function apply(projectRoot: string, vendors: VendorId[]): void {
	setProjectVendors(projectRoot, vendors);
	ensureProjectMemoryHooks(projectRoot);
	listVendors(projectRoot);
}

function listVendors(projectRoot: string): void {
	const enabled = new Set(currentVendors(projectRoot));
	const probes = detectVendors();
	consola.info("");
	consola.info(pc.bold(`AI tools for ${pc.dim(projectRoot)}`));
	consola.info("");
	for (const p of probes) {
		const mark = enabled.has(p.id) ? pc.green("◉") : pc.dim("○");
		const state = p.configured
			? pc.green("✓ detected")
			: p.installed
				? pc.yellow("installed")
				: pc.dim("not found");
		consola.info(`  ${mark} ${p.display.padEnd(16)} ${pc.dim(p.id.padEnd(12))} ${state}`);
	}
	consola.info("");
	consola.info(
		`${pc.dim("add/remove:")} ${pc.cyan("immorterm vendors add codex")} · ${pc.cyan("immorterm vendors remove claudeCode")}`,
	);
}

export const vendorsCommand = defineCommand({
	meta: {
		name: "vendors",
		description: "List or change which AI tools ImmorTerm hooks into (per project)",
	},
	subCommands: {
		list: defineCommand({
			meta: { name: "list", description: "Show every AI tool with its detection and enabled state" },
			run() {
				listVendors(process.cwd());
			},
		}),
		add: defineCommand({
			meta: { name: "add", description: "Enable AI tool(s) for this project" },
			args: { ids: { type: "positional", description: "Comma-separated vendor ids", required: true } },
			run({ args }) {
				const ids = parseIds(args.ids as string);
				if (!ids) return;
				const cwd = process.cwd();
				apply(cwd, [...new Set([...currentVendors(cwd), ...ids])]);
			},
		}),
		remove: defineCommand({
			meta: {
				name: "remove",
				description: "Disable AI tool(s) and delete the config files they own",
			},
			args: { ids: { type: "positional", description: "Comma-separated vendor ids", required: true } },
			run({ args }) {
				const ids = parseIds(args.ids as string);
				if (!ids) return;
				const cwd = process.cwd();
				apply(cwd, currentVendors(cwd).filter((v) => !ids.includes(v)));
			},
		}),
		set: defineCommand({
			meta: { name: "set", description: "Replace the selection with exactly these AI tools" },
			args: { ids: { type: "positional", description: "Comma-separated vendor ids", required: true } },
			run({ args }) {
				const ids = parseIds(args.ids as string);
				if (!ids) return;
				apply(process.cwd(), ids);
			},
		}),
	},
	run({ rawArgs }) {
		// Bare `immorterm vendors` lists; citty still calls the parent run() when
		// a subcommand matched, so only act when nothing followed.
		if (rawArgs.length === 0) {
			listVendors(process.cwd());
		}
	},
});
