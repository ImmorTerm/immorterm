#!/usr/bin/env node
/**
 * postinstall — Auto-install VS Code extension when installing immorterm via npm/npx
 *
 * Runs silently: if VS Code isn't detected or install fails, exits quietly.
 * Never blocks npm install completion.
 *
 * Uses execFile (not exec) to avoid shell injection — all arguments are passed as arrays.
 */

import { execFile } from "node:child_process";

// Kept in sync with the source of truth in libs/services/src/versions.ts.
// This is a dependency-free npm postinstall script, so it can't import it.
const EXTENSION_ID = "immorterm.immorterm-terminal";
const VSCODE_BINARIES = ["code", "code-insiders", "cursor"];
const TIMEOUT = 15000;

function execFilePromise(cmd, args) {
	return new Promise((resolve, reject) => {
		execFile(cmd, args, { timeout: TIMEOUT }, (error, stdout) => {
			if (error) reject(error);
			else resolve(stdout);
		});
	});
}

async function main() {
	// Skip in CI environments
	if (process.env.CI || process.env.CONTINUOUS_INTEGRATION) return;

	// Detect VS Code
	let binary = null;
	for (const bin of VSCODE_BINARIES) {
		try {
			await execFilePromise(bin, ["--version"]);
			binary = bin;
			break;
		} catch { /* not found */ }
	}
	if (!binary) return;

	// Check if already installed
	try {
		const extensions = await execFilePromise(binary, ["--list-extensions"]);
		if (extensions.toLowerCase().includes(EXTENSION_ID.toLowerCase())) return;
	} catch { return; }

	// Install extension
	try {
		console.log(`\n  ImmorTerm: Installing VS Code extension...`);
		await execFilePromise(binary, ["--install-extension", EXTENSION_ID]);
		console.log(`  ImmorTerm: Extension installed! Reload VS Code to activate.\n`);
	} catch {
		console.log(`  ImmorTerm: Auto-install skipped. Run: ${binary} --install-extension ${EXTENSION_ID}\n`);
	}
}

main().catch(() => {});
