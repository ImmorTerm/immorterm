import { createReadStream } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { createInterface } from "node:readline";
import {
	type GridSnapshot,
	GridSnapshotSchema,
	type ScrollbackDump,
	ScrollbackDumpSchema,
} from "./schema.js";

// ---------------------------------------------------------------------------
// Generic NDJSON line reader (async generator)
// ---------------------------------------------------------------------------
// Streams an NDJSON file line-by-line, yielding parsed JSON objects.
// Lines that fail JSON.parse or Zod validation (when a schema is provided)
// are silently skipped with a console.warn.
// ---------------------------------------------------------------------------

export async function* readNdjson<T>(
	path: string,
	schema?: { safeParse: (data: unknown) => { success: boolean; data?: T } },
): AsyncGenerator<T> {
	const input = createReadStream(path, { encoding: "utf-8" });
	const rl = createInterface({ input, crlfDelay: Number.POSITIVE_INFINITY });

	let lineNumber = 0;
	for await (const line of rl) {
		lineNumber++;
		const trimmed = line.trim();
		if (trimmed.length === 0) continue;

		let parsed: unknown;
		try {
			parsed = JSON.parse(trimmed);
		} catch {
			console.warn(`@immorterm/terminal-logs: invalid JSON at ${path}:${lineNumber}, skipping`);
			continue;
		}

		if (schema) {
			const result = schema.safeParse(parsed);
			if (!result.success) {
				console.warn(
					`@immorterm/terminal-logs: schema validation failed at ${path}:${lineNumber}, skipping`,
				);
				continue;
			}
			yield result.data as T;
		} else {
			yield parsed as T;
		}
	}
}

// ---------------------------------------------------------------------------
// Read all grid snapshots from a .grid.jsonl file
// ---------------------------------------------------------------------------

export async function readGridLog(path: string): Promise<GridSnapshot[]> {
	const snapshots: GridSnapshot[] = [];
	for await (const snapshot of readNdjson<GridSnapshot>(path, GridSnapshotSchema)) {
		snapshots.push(snapshot);
	}
	return snapshots;
}

// ---------------------------------------------------------------------------
// Read only the last snapshot (reads from end for efficiency)
// ---------------------------------------------------------------------------
// Strategy: for files under 10MB, read the whole file and find the last
// snapshot line. For larger files, read trailing chunks until we find one.
// ---------------------------------------------------------------------------

const LARGE_FILE_THRESHOLD = 10 * 1024 * 1024; // 10MB
const TAIL_CHUNK_SIZE = 64 * 1024; // 64KB chunks when reading from end

export async function readLastSnapshot(path: string): Promise<GridSnapshot | null> {
	const fileStat = await stat(path).catch(() => null);
	if (!fileStat || fileStat.size === 0) return null;

	if (fileStat.size <= LARGE_FILE_THRESHOLD) {
		return readLastSnapshotSmallFile(path);
	}
	return readLastSnapshotLargeFile(path, fileStat.size);
}

async function readLastSnapshotSmallFile(path: string): Promise<GridSnapshot | null> {
	const content = await readFile(path, "utf-8");
	const lines = content.split("\n");

	// Walk backwards to find the last snapshot line
	for (let i = lines.length - 1; i >= 0; i--) {
		const trimmed = lines[i]!.trim();
		if (trimmed.length === 0) continue;
		if (!trimmed.includes('"type":"snapshot"') && !trimmed.includes('"type": "snapshot"')) {
			continue;
		}

		try {
			const parsed = JSON.parse(trimmed);
			const result = GridSnapshotSchema.safeParse(parsed);
			if (result.success) return result.data;
		} catch {
			// Not valid JSON, try next line
		}
	}

	return null;
}

async function readLastSnapshotLargeFile(
	path: string,
	fileSize: number,
): Promise<GridSnapshot | null> {
	const { open } = await import("node:fs/promises");
	const fh = await open(path, "r");

	try {
		let offset = fileSize;
		let remainder = "";

		while (offset > 0) {
			const chunkSize = Math.min(TAIL_CHUNK_SIZE, offset);
			offset -= chunkSize;

			const buffer = Buffer.alloc(chunkSize);
			await fh.read(buffer, 0, chunkSize, offset);

			const chunk = buffer.toString("utf-8") + remainder;
			const lines = chunk.split("\n");

			// The first element may be a partial line — save it as remainder
			remainder = lines[0] ?? "";

			// Walk backwards through complete lines
			for (let i = lines.length - 1; i >= 1; i--) {
				const trimmed = lines[i]!.trim();
				if (trimmed.length === 0) continue;
				if (!trimmed.includes('"type":"snapshot"') && !trimmed.includes('"type": "snapshot"')) {
					continue;
				}

				try {
					const parsed = JSON.parse(trimmed);
					const result = GridSnapshotSchema.safeParse(parsed);
					if (result.success) return result.data;
				} catch {
					// Not valid JSON, try next line
				}
			}
		}

		// Check the very first line (remainder) if we exhausted the file
		if (remainder.trim().length > 0) {
			try {
				const parsed = JSON.parse(remainder.trim());
				const result = GridSnapshotSchema.safeParse(parsed);
				if (result.success) return result.data;
			} catch {
				// Not valid JSON
			}
		}

		return null;
	} finally {
		await fh.close();
	}
}

// ---------------------------------------------------------------------------
// Read the latest scrollback dump
// ---------------------------------------------------------------------------

export async function readScrollbackDump(path: string): Promise<ScrollbackDump | null> {
	const fileStat = await stat(path).catch(() => null);
	if (!fileStat || fileStat.size === 0) return null;

	const content = await readFile(path, "utf-8");
	const lines = content.split("\n");

	// Walk backwards to find the last scrollback line
	for (let i = lines.length - 1; i >= 0; i--) {
		const trimmed = lines[i]!.trim();
		if (trimmed.length === 0) continue;

		try {
			const parsed = JSON.parse(trimmed);
			const result = ScrollbackDumpSchema.safeParse(parsed);
			if (result.success) return result.data;
		} catch {
			// Not valid JSON, try next line
		}
	}

	return null;
}
