#!/usr/bin/env node
/**
 * postinstall — Resolve the correct platform binary for immorterm-memory
 *
 * npm installs the matching platform package via optionalDependencies + os/cpu
 * fields. This script finds the installed binary and copies it to our bin/ dir.
 *
 * Follows the esbuild binary distribution pattern.
 */

const { existsSync, mkdirSync, copyFileSync, chmodSync } = require("fs");
const { join, dirname } = require("path");

const BINARY_NAME = process.platform === "win32" ? "immorterm-memory.exe" : "immorterm-memory";

const PLATFORM_PACKAGES = {
	"darwin-arm64": "immorterm-memory-darwin-arm64",
	"darwin-x64": "immorterm-memory-darwin-x64",
	"linux-x64": "immorterm-memory-linux-x64",
	"linux-arm64": "immorterm-memory-linux-arm64",
};

function main() {
	const platformKey = `${process.platform}-${process.arch}`;
	const pkg = PLATFORM_PACKAGES[platformKey];

	if (!pkg) {
		console.error(
			`immorterm-memory: Unsupported platform ${platformKey}. ` +
			`Supported: ${Object.keys(PLATFORM_PACKAGES).join(", ")}`
		);
		process.exit(0); // Don't fail npm install
	}

	// Try to find the platform package's binary
	let sourcePath;
	try {
		const pkgDir = dirname(require.resolve(`${pkg}/package.json`));
		sourcePath = join(pkgDir, "bin", BINARY_NAME);
	} catch {
		// Platform package not installed (npm may have skipped it)
		console.error(
			`immorterm-memory: Platform package ${pkg} not found. ` +
			`Install manually: npm install ${pkg}`
		);
		process.exit(0);
	}

	if (!existsSync(sourcePath)) {
		console.error(`immorterm-memory: Binary not found at ${sourcePath}`);
		process.exit(0);
	}

	// Copy binary to our bin/ directory
	const destDir = join(__dirname, "bin");
	const destPath = join(destDir, BINARY_NAME);

	mkdirSync(destDir, { recursive: true });
	copyFileSync(sourcePath, destPath);
	chmodSync(destPath, 0o755);
}

main();
