/**
 * immorterm install <component> — Install ImmorTerm components
 *
 * Currently supports:
 *   extension [--pre]  — Install VS Code extension (stable or pre-release)
 */

import { defineCommand } from "citty";
import consola from "consola";
import pc from "picocolors";
import {
	detectVsCode,
	installExtension,
	isExtensionInstalled,
	getExtensionVersion,
} from "@immorterm/services";

const installExtensionCommand = defineCommand({
	meta: {
		name: "extension",
		description: "Install the ImmorTerm VS Code extension",
	},
	args: {
		pre: {
			type: "boolean",
			description: "Install pre-release version",
			default: false,
		},
		force: {
			type: "boolean",
			description: "Reinstall even if already installed",
			default: false,
		},
	},
	async run({ args }) {
		const preRelease = args.pre as boolean;
		const force = args.force as boolean;

		// Detect VS Code
		const vscode = await detectVsCode();
		if (!vscode.available || !vscode.binary) {
			consola.error("VS Code CLI not found.");
			consola.info("Make sure the `code` command is in your PATH.");
			consola.info(`  VS Code → Command Palette → "Shell Command: Install 'code' command in PATH"`);
			return;
		}

		consola.info(`Detected: ${pc.cyan(vscode.binary)} (${vscode.version ?? "unknown version"})`);

		// Check current state
		if (!force) {
			const version = await getExtensionVersion(vscode.binary);
			if (version) {
				consola.success(`Extension already installed (v${version})`);
				consola.info(`  Use ${pc.cyan("--force")} to reinstall`);
				return;
			}
		}

		// Install
		const tag = preRelease ? "pre-release" : "stable";
		consola.start(`Installing ImmorTerm extension (${tag})...`);

		const result = await installExtension(
			{ binary: vscode.binary, preRelease, force },
			{
				info: (msg) => consola.info(msg),
				warn: (msg) => consola.warn(msg),
				error: (msg) => consola.error(msg),
			},
		);

		if (result.success) {
			consola.success(`Extension installed! Reload VS Code to activate.`);
		} else {
			consola.error(`Installation failed: ${result.error}`);
			consola.info(`  Manual: ${pc.cyan(`${vscode.binary} --install-extension immorterm.immorterm-extension${preRelease ? " --pre-release" : ""}`)}`);
		}
	},
});

export const installCommand = defineCommand({
	meta: {
		name: "install",
		description: "Install ImmorTerm components",
	},
	subCommands: {
		extension: installExtensionCommand,
	},
});
