/**
 * immorterm config — Get/set config values
 */

import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import { readGlobalConfig, writeGlobalConfig } from "@immorterm/config";

const getCommand = defineCommand({
	meta: { name: "get", description: "Read a config value" },
	args: {
		key: {
			type: "positional",
			description: "Config key (dot notation, e.g., defaults.services.memory.enabled)",
			required: true,
		},
	},
	async run({ args }) {
		const config = readGlobalConfig();
		const value = getNestedValue(config, args.key);
		if (value === undefined) {
			consola.warn(`Key not found: ${args.key}`);
		} else {
			consola.info(typeof value === "object" ? JSON.stringify(value, null, 2) : String(value));
		}
	},
});

const setCommand = defineCommand({
	meta: { name: "set", description: "Write a config value" },
	args: {
		key: {
			type: "positional",
			description: "Config key (dot notation)",
			required: true,
		},
		value: {
			type: "positional",
			description: "Value to set (supports true/false/null/numbers)",
			required: true,
		},
	},
	async run({ args }) {
		const config = readGlobalConfig();
		const parsed = parseValue(args.value);

		if (setNestedValue(config, args.key, parsed)) {
			writeGlobalConfig(config);
			consola.success(`${pc.bold(args.key)} = ${pc.green(String(parsed))}`);
		} else {
			consola.error(`Could not set key: ${args.key}`);
		}
	},
});

export const configCommand = defineCommand({
	meta: {
		name: "config",
		description: "Get or set config values",
	},
	subCommands: {
		get: getCommand,
		set: setCommand,
	},
});

// ── Helpers ────────────────────────────────────────────────────────

function getNestedValue(obj: any, path: string): any {
	return path.split(".").reduce((curr, key) => curr?.[key], obj);
}

function setNestedValue(obj: any, path: string, value: any): boolean {
	const keys = path.split(".");
	let curr = obj;
	for (let i = 0; i < keys.length - 1; i++) {
		const key = keys[i]!;
		// Auto-vivify missing intermediate objects so paths like
		// `defaults.services.digest.provider` work even when the wizard
		// hasn't materialised the `digest` block yet (Phase A T11).
		if (curr[key] === undefined || curr[key] === null) {
			curr[key] = {};
		} else if (typeof curr[key] !== "object" || Array.isArray(curr[key])) {
			return false;
		}
		curr = curr[key];
	}
	const lastKey = keys[keys.length - 1]!;
	curr[lastKey] = value;
	return true;
}

function parseValue(input: string): any {
	if (input === "true") return true;
	if (input === "false") return false;
	if (input === "null") return null;
	const num = Number(input);
	if (!Number.isNaN(num)) return num;
	return input;
}
