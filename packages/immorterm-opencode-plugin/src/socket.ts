/**
 * Hub transport for ImmorTerm hook envelopes.
 *
 * Phase A decision: file-drop pattern (NOT a new hub HTTP route).
 * The opencode plugin writes envelopes as JSON files into
 *   <project>/.immorterm/hooks/inbox/opencode-<ts>-<rand>.json
 * The existing hooks daemon (or a follow-up watcher task) picks them up
 * and pipes them to the matching hook script — same entrypoint other
 * vendors use.
 *
 * Why files, not the Unix socket on hub.sock: T9's session-link endpoint
 * is for session announcements only. A general hook-forwarding route is
 * out of scope for Phase A. Files are observable, atomic (rename), and
 * survive hub restarts — perfect for testing.
 */

import { mkdir, rename, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { randomBytes } from "node:crypto";

export interface PostToHubOptions {
	/**
	 * Project root. The inbox lives at `<projectDir>/.immorterm/hooks/inbox/`.
	 */
	projectDir: string;
	/**
	 * Override the inbox directory entirely (used by tests).
	 */
	inboxDir?: string;
}

/**
 * Writes the envelope to the inbox as `opencode-<ts>-<rand>.json`.
 *
 * Atomic: writes to a `.tmp` file first, then renames. Watchers that key on
 * `*.json` (and ignore `.tmp`) won't see partial files.
 *
 * Returns the absolute path of the file written, useful for tests + logs.
 */
export async function postToHub(
	envelope: Record<string, unknown>,
	opts: PostToHubOptions,
): Promise<string> {
	const inbox = opts.inboxDir ?? resolve(opts.projectDir, ".immorterm/hooks/inbox");
	await mkdir(inbox, { recursive: true });

	const ts = Date.now();
	const rand = randomBytes(4).toString("hex");
	const filename = `opencode-${ts}-${rand}.json`;
	const finalPath = resolve(inbox, filename);
	const tmpPath = `${finalPath}.tmp`;

	const body = JSON.stringify(envelope);
	await writeFile(tmpPath, body, { encoding: "utf8", mode: 0o600 });
	await rename(tmpPath, finalPath);

	return finalPath;
}

export function inboxDirFor(projectDir: string): string {
	return resolve(projectDir, ".immorterm/hooks/inbox");
}

// dirname kept for forward compatibility (e.g., if we add a sibling write)
export const __internal = { dirname };
