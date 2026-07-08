/**
 * Version Resolution — query versions from all sources in parallel
 *
 * Checks npm, VS Code Marketplace, GitHub, and local binaries for each component.
 */

import { execFile } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import {
	MEMORY_BINARY,
	findBinary,
	findLatestMemoryRelease,
	getInstalledMemoryTag,
} from "./memory.js";

const execFileAsync = promisify(execFile);

export interface ComponentVersion {
	name: string;
	current: string | null;
	latest: string | null;
	updateAvailable: boolean;
	source: "npm" | "github" | "vscode" | "local";
}

/** CLI package name on npm */
const CLI_PACKAGE = "immorterm";
/** Extension publisher.name on VS Code Marketplace — the published id is
 * immorterm-EXTENSION (apps/extension/package.json name), not immorterm. */
const EXTENSION_ID = "immorterm.immorterm-extension";
/** GitHub repo for releases */
const GITHUB_REPO = "ImmorTerm/immorterm";

let cliVersionCache: string | null | undefined;

/**
 * Get current CLI version by reading the `immorterm` package.json at runtime.
 * Walks up from the executed script (dist/cli.mjs when npm-installed,
 * src/main.ts in dev) and from this module. Never hardcoded — CI bumps
 * package.json only, so anything else drifts.
 */
export function getCliVersion(): string | null {
	if (cliVersionCache !== undefined) return cliVersionCache;
	const starts = [process.argv[1], fileURLToPath(import.meta.url)].filter(Boolean) as string[];
	for (const start of starts) {
		let dir = path.dirname(path.resolve(start));
		for (let depth = 0; depth < 6; depth++) {
			try {
				const pkg = JSON.parse(
					fs.readFileSync(path.join(dir, "package.json"), "utf-8"),
				) as { name?: string; version?: string };
				if (pkg.name === CLI_PACKAGE && pkg.version) {
					cliVersionCache = pkg.version;
					return pkg.version;
				}
			} catch {}
			const parent = path.dirname(dir);
			if (parent === dir) break;
			dir = parent;
		}
	}
	cliVersionCache = null;
	return null;
}

/** Query npm registry for the latest version of a package */
async function getNpmLatest(pkg: string): Promise<string | null> {
	try {
		const res = await fetch(`https://registry.npmjs.org/${pkg}/latest`, {
			signal: AbortSignal.timeout(5000),
		});
		if (!res.ok) return null;
		const data = (await res.json()) as { version?: string };
		return data.version ?? null;
	} catch {
		return null;
	}
}

/** Get installed VS Code extension version */
async function getInstalledExtensionVersion(): Promise<string | null> {
	try {
		const { stdout } = await execFileAsync("code", [
			"--list-extensions", "--show-versions",
		], { timeout: 10000 });
		const line = stdout.split("\n").find((l) => l.toLowerCase().startsWith(EXTENSION_ID.toLowerCase()));
		if (!line) return null;
		// Format: publisher.name@version
		const match = line.match(/@(.+)$/);
		return match?.[1] ?? null;
	} catch {
		return null;
	}
}

/** Get latest extension version from VS Code Marketplace API */
async function getMarketplaceLatest(): Promise<string | null> {
	try {
		const res = await fetch(
			"https://marketplace.visualstudio.com/_apis/public/gallery/extensionquery",
			{
				method: "POST",
				headers: {
					"Content-Type": "application/json",
					Accept: "application/json;api-version=6.0-preview.1",
				},
				body: JSON.stringify({
					filters: [{
						criteria: [{ filterType: 7, value: EXTENSION_ID }],
					}],
					flags: 0x200, // IncludeVersions
				}),
				signal: AbortSignal.timeout(5000),
			},
		);
		if (!res.ok) return null;
		const data = await res.json() as any;
		const ext = data?.results?.[0]?.extensions?.[0];
		return ext?.versions?.[0]?.version ?? null;
	} catch {
		return null;
	}
}

/** Get latest GitHub release tag matching a prefix */
async function getGitHubLatest(tagPrefix: string): Promise<string | null> {
	try {
		const res = await fetch(
			`https://api.github.com/repos/${GITHUB_REPO}/releases`,
			{
				headers: { Accept: "application/vnd.github.v3+json" },
				signal: AbortSignal.timeout(5000),
			},
		);
		if (!res.ok) return null;
		const releases = (await res.json()) as Array<{ tag_name: string; prerelease: boolean }>;
		const matching = releases.find(
			(r) => r.tag_name.startsWith(tagPrefix) && !r.prerelease,
		);
		return matching?.tag_name?.replace(tagPrefix, "") ?? null;
	} catch {
		return null;
	}
}

/** Get native memory binary version */
async function getMemoryBinaryVersion(): Promise<string | null> {
	try {
		const { stdout } = await execFileAsync(MEMORY_BINARY, ["--version"], { timeout: 5000 });
		const match = stdout.trim().match(/(\d+\.\d+\.\d+)/);
		return match?.[1] ?? null;
	} catch {
		return null;
	}
}

/** Get immorterm-ai (Rust) binary version */
async function getAiBinaryVersion(): Promise<string | null> {
	try {
		const { stdout } = await execFileAsync("immorterm-ai", ["--version"], { timeout: 5000 });
		const match = stdout.trim().match(/(\d+\.\d+\.\d+)/);
		return match?.[1] ?? null;
	} catch {
		// Try alternate binary name
		try {
			const { stdout } = await execFileAsync("immorterm-rust", ["--version"], { timeout: 5000 });
			const match = stdout.trim().match(/(\d+\.\d+\.\d+)/);
			return match?.[1] ?? null;
		} catch {
			return null;
		}
	}
}

/** True when `latest` is a strictly newer x.y.z than `current`. Non-semver input → false. */
export function compareVersions(current: string | null, latest: string | null): boolean {
	if (!current || !latest) return false;
	const c = current.split(".").map(Number);
	const l = latest.split(".").map(Number);
	// Date-stamped release tags ("prod-2026-04-09.2") aren't semver — never claim an update
	if ([...c, ...l].some(Number.isNaN)) return false;
	for (let i = 0; i < Math.max(c.length, l.length); i++) {
		const cv = c[i] ?? 0;
		const lv = l[i] ?? 0;
		if (lv > cv) return true;
		if (lv < cv) return false;
	}
	return false;
}

/** Latest npm version when newer than the running CLI, else null. */
export async function checkCliUpdate(): Promise<string | null> {
	const current = getCliVersion();
	const latest = await getNpmLatest(CLI_PACKAGE);
	return compareVersions(current, latest) ? latest : null;
}

/** Query all component versions in parallel */
export async function getAllVersions(): Promise<ComponentVersion[]> {
	const [
		cliLatest,
		extensionCurrent,
		extensionLatest,
		memoryBinaryVersion,
		memoryLatest,
		aiCurrent,
		aiLatest,
	] = await Promise.all([
		getNpmLatest(CLI_PACKAGE),
		getInstalledExtensionVersion(),
		getMarketplaceLatest(),
		getMemoryBinaryVersion(),
		findLatestMemoryRelease().then((r) => r?.tag ?? null).catch(() => null),
		getAiBinaryVersion(),
		// CI tags are ai-prod-YYYY-MM-DD.N (20-promote-prod.yml)
		getGitHubLatest("ai-prod-"),
	]);

	const cliCurrent = getCliVersion();
	// Memory binary has no --version; the install stamp (release tag) is the real identity.
	// Unstamped-but-present binaries report "unknown" so `upgrade --force memory` can
	// still re-download and bootstrap the stamp.
	const memoryTag = getInstalledMemoryTag();
	const memoryCurrent = memoryTag ?? memoryBinaryVersion ?? (findBinary() ? "unknown" : null);

	return [
		{
			name: "CLI",
			current: cliCurrent,
			latest: cliLatest,
			updateAvailable: compareVersions(cliCurrent, cliLatest),
			source: "npm",
		},
		{
			name: "Extension",
			current: extensionCurrent,
			latest: extensionLatest,
			updateAvailable: compareVersions(extensionCurrent, extensionLatest),
			source: "vscode",
		},
		{
			name: "Memory",
			current: memoryCurrent,
			latest: memoryLatest,
			// Tags are dates, not semver — any tag mismatch vs the install stamp means "re-download"
			updateAvailable: memoryTag !== null && memoryLatest !== null && memoryTag !== memoryLatest,
			source: "github",
		},
		{
			name: "AI Binary",
			current: aiCurrent,
			latest: aiLatest,
			updateAvailable: compareVersions(aiCurrent, aiLatest),
			source: "github",
		},
	];
}
