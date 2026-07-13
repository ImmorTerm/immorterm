/**
 * VS Code Detection & Extension Management — IDE-Independent
 *
 * Detects VS Code CLI availability and manages extension installation.
 * Works with `code`, `code-insiders`, and Cursor.
 */

import { execFile } from "node:child_process";
import { promisify } from "node:util";
import type { Logger } from "./types.js";
// Single source of truth for the extension id (see versions.ts).
import { EXTENSION_ID } from "./versions.js";

const execFileAsync = promisify(execFile);

/** Possible VS Code CLI binaries, in preference order */
const VSCODE_BINARIES = ["code", "code-insiders", "cursor"] as const;

export type VsCodeBinary = (typeof VSCODE_BINARIES)[number];

export interface VsCodeDetection {
	available: boolean;
	binary: VsCodeBinary | null;
	version: string | null;
}

/** Detect if VS Code CLI is available */
export async function detectVsCode(): Promise<VsCodeDetection> {
	for (const bin of VSCODE_BINARIES) {
		try {
			const { stdout } = await execFileAsync(bin, ["--version"], { timeout: 5000 });
			const version = stdout.trim().split("\n")[0] ?? null;
			return { available: true, binary: bin, version };
		} catch {
			continue;
		}
	}
	return { available: false, binary: null, version: null };
}

/** Check if the ImmorTerm extension is already installed */
export async function isExtensionInstalled(binary: VsCodeBinary = "code"): Promise<boolean> {
	try {
		const { stdout } = await execFileAsync(binary, ["--list-extensions"], { timeout: 10000 });
		return stdout.toLowerCase().includes(EXTENSION_ID.toLowerCase());
	} catch {
		return false;
	}
}

/** Get installed extension version, or null if not installed */
export async function getExtensionVersion(binary: VsCodeBinary = "code"): Promise<string | null> {
	try {
		const { stdout } = await execFileAsync(binary, [
			"--list-extensions", "--show-versions",
		], { timeout: 10000 });
		const line = stdout.split("\n").find((l) => l.toLowerCase().startsWith(EXTENSION_ID.toLowerCase()));
		if (!line) return null;
		const match = line.match(/@(.+)$/);
		return match?.[1] ?? null;
	} catch {
		return null;
	}
}

export interface ExtensionInstallResult {
	success: boolean;
	alreadyInstalled: boolean;
	error?: string;
}

/** Install the ImmorTerm VS Code extension */
export async function installExtension(
	options: { preRelease?: boolean; binary?: VsCodeBinary; force?: boolean } = {},
	log?: Logger,
): Promise<ExtensionInstallResult> {
	const binary = options.binary ?? "code";

	// Check if already installed (unless force)
	if (!options.force) {
		const installed = await isExtensionInstalled(binary);
		if (installed) {
			return { success: true, alreadyInstalled: true };
		}
	}

	const args = ["--install-extension", EXTENSION_ID];
	if (options.preRelease) args.push("--pre-release");
	if (options.force) args.push("--force");

	try {
		log?.info(`Installing extension via ${binary}...`);
		await execFileAsync(binary, args, { timeout: 60000 });
		return { success: true, alreadyInstalled: false };
	} catch (error) {
		const msg = String(error);
		return { success: false, alreadyInstalled: false, error: msg };
	}
}

/** Auto-install extension if VS Code is detected and extension isn't already installed */
export async function autoInstallExtension(log?: Logger): Promise<boolean> {
	const vscode = await detectVsCode();
	if (!vscode.available || !vscode.binary) {
		return false;
	}

	const installed = await isExtensionInstalled(vscode.binary);
	if (installed) {
		log?.info("VS Code extension already installed");
		return true;
	}

	log?.info(`VS Code detected (${vscode.binary}) — installing ImmorTerm extension...`);
	const result = await installExtension({ binary: vscode.binary }, log);
	if (result.success) {
		log?.info("VS Code extension installed successfully");
	} else {
		log?.warn(`Extension install failed: ${result.error}`);
		log?.info(`  Manual: ${vscode.binary} --install-extension ${EXTENSION_ID}`);
	}
	return result.success;
}
