import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { z } from "zod";
import { readGridLog, readLastSnapshot, readNdjson, readScrollbackDump } from "../reader.js";
import { GridSnapshotSchema, ScrollbackDumpSchema } from "../schema.js";

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

const makeSnapshot = (ts: number, trigger = "prompt") =>
	JSON.stringify({
		v: 1,
		type: "snapshot",
		ts,
		trigger,
		cols: 120,
		rows: 36,
		cursor: { col: 0, row: 0 },
		cwd: "/tmp",
		exit_code: 0,
		grid: [
			{
				row: 0,
				runs: [
					{ t: `snapshot@${ts}`, fg: "default", bg: "default", a: 0 },
				],
			},
		],
		sb_lines: 0,
		sb_hash: `hash-${ts}`,
	});

const makeScrollback = (ts: number) =>
	JSON.stringify({
		v: 1,
		type: "scrollback",
		ts,
		lines: [
			{
				runs: [
					{ t: `scrollback line at ${ts}`, fg: "default", bg: "default", a: 0 },
				],
			},
		],
		hash: `sb-hash-${ts}`,
	});

// ---------------------------------------------------------------------------
// Temp directory management
// ---------------------------------------------------------------------------

let tmpDir: string;

beforeAll(async () => {
	tmpDir = await mkdtemp(join(tmpdir(), "terminal-logs-test-"));
});

afterAll(async () => {
	await rm(tmpDir, { recursive: true, force: true });
});

// ---------------------------------------------------------------------------
// readNdjson
// ---------------------------------------------------------------------------

describe("readNdjson", () => {
	it("reads all lines from an NDJSON file", async () => {
		const filePath = join(tmpDir, "basic.jsonl");
		await writeFile(filePath, '{"a":1}\n{"a":2}\n{"a":3}\n');

		const items: unknown[] = [];
		for await (const item of readNdjson(filePath)) {
			items.push(item);
		}
		expect(items).toEqual([{ a: 1 }, { a: 2 }, { a: 3 }]);
	});

	it("skips blank lines", async () => {
		const filePath = join(tmpDir, "blanks.jsonl");
		await writeFile(filePath, '{"x":1}\n\n\n{"x":2}\n\n');

		const items: unknown[] = [];
		for await (const item of readNdjson(filePath)) {
			items.push(item);
		}
		expect(items).toEqual([{ x: 1 }, { x: 2 }]);
	});

	it("skips invalid JSON lines", async () => {
		const filePath = join(tmpDir, "bad-json.jsonl");
		await writeFile(filePath, '{"ok":true}\nnot-json\n{"ok":false}\n');

		const items: unknown[] = [];
		for await (const item of readNdjson(filePath)) {
			items.push(item);
		}
		expect(items).toEqual([{ ok: true }, { ok: false }]);
	});

	it("validates with a Zod schema when provided", async () => {
		const filePath = join(tmpDir, "validated.jsonl");
		await writeFile(filePath, '{"name":"alice","age":30}\n{"name":"bob"}\n{"name":"charlie","age":25}\n');

		const PersonSchema = z.object({ name: z.string(), age: z.number() });
		const items: z.infer<typeof PersonSchema>[] = [];
		for await (const item of readNdjson(filePath, PersonSchema)) {
			items.push(item);
		}
		// Second line fails validation (missing age), should be skipped
		expect(items).toHaveLength(2);
		expect(items[0]!.name).toBe("alice");
		expect(items[1]!.name).toBe("charlie");
	});

	it("handles empty file", async () => {
		const filePath = join(tmpDir, "empty.jsonl");
		await writeFile(filePath, "");

		const items: unknown[] = [];
		for await (const item of readNdjson(filePath)) {
			items.push(item);
		}
		expect(items).toHaveLength(0);
	});
});

// ---------------------------------------------------------------------------
// readGridLog
// ---------------------------------------------------------------------------

describe("readGridLog", () => {
	it("reads all valid snapshots", async () => {
		const filePath = join(tmpDir, "grid.jsonl");
		const content = [
			makeSnapshot(1000),
			makeSnapshot(2000, "periodic"),
			makeScrollback(1500), // not a snapshot — will be skipped
			makeSnapshot(3000, "shutdown"),
		].join("\n") + "\n";
		await writeFile(filePath, content);

		const snapshots = await readGridLog(filePath);
		expect(snapshots).toHaveLength(3);
		expect(snapshots[0]!.ts).toBe(1000);
		expect(snapshots[1]!.trigger).toBe("periodic");
		expect(snapshots[2]!.trigger).toBe("shutdown");
	});

	it("returns empty array for file with no snapshots", async () => {
		const filePath = join(tmpDir, "no-snapshots.jsonl");
		await writeFile(filePath, makeScrollback(1000) + "\n");

		const snapshots = await readGridLog(filePath);
		expect(snapshots).toHaveLength(0);
	});
});

// ---------------------------------------------------------------------------
// readLastSnapshot
// ---------------------------------------------------------------------------

describe("readLastSnapshot", () => {
	it("returns the last snapshot in a mixed file", async () => {
		const filePath = join(tmpDir, "last-snap.jsonl");
		const content = [
			makeSnapshot(1000),
			makeScrollback(1500),
			makeSnapshot(2000, "periodic"),
			makeScrollback(2500),
		].join("\n") + "\n";
		await writeFile(filePath, content);

		const last = await readLastSnapshot(filePath);
		expect(last).not.toBeNull();
		expect(last!.ts).toBe(2000);
		expect(last!.trigger).toBe("periodic");
	});

	it("returns null for a file with only scrollback dumps", async () => {
		const filePath = join(tmpDir, "only-sb.jsonl");
		await writeFile(filePath, makeScrollback(1000) + "\n" + makeScrollback(2000) + "\n");

		const last = await readLastSnapshot(filePath);
		expect(last).toBeNull();
	});

	it("returns null for non-existent file", async () => {
		const last = await readLastSnapshot(join(tmpDir, "no-such-file.jsonl"));
		expect(last).toBeNull();
	});

	it("returns null for empty file", async () => {
		const filePath = join(tmpDir, "empty-snap.jsonl");
		await writeFile(filePath, "");

		const last = await readLastSnapshot(filePath);
		expect(last).toBeNull();
	});

	it("returns the only snapshot when it is the last line", async () => {
		const filePath = join(tmpDir, "single-snap.jsonl");
		await writeFile(filePath, makeSnapshot(5000, "manual") + "\n");

		const last = await readLastSnapshot(filePath);
		expect(last).not.toBeNull();
		expect(last!.ts).toBe(5000);
		expect(last!.trigger).toBe("manual");
	});
});

// ---------------------------------------------------------------------------
// readScrollbackDump
// ---------------------------------------------------------------------------

describe("readScrollbackDump", () => {
	it("returns the last scrollback dump", async () => {
		const filePath = join(tmpDir, "sb.jsonl");
		const content = [
			makeScrollback(1000),
			makeSnapshot(1500),
			makeScrollback(2000),
		].join("\n") + "\n";
		await writeFile(filePath, content);

		const dump = await readScrollbackDump(filePath);
		expect(dump).not.toBeNull();
		expect(dump!.hash).toBe("sb-hash-2000");
	});

	it("returns null for non-existent file", async () => {
		const dump = await readScrollbackDump(join(tmpDir, "missing.jsonl"));
		expect(dump).toBeNull();
	});

	it("returns null for empty file", async () => {
		const filePath = join(tmpDir, "empty-sb.jsonl");
		await writeFile(filePath, "");

		const dump = await readScrollbackDump(filePath);
		expect(dump).toBeNull();
	});

	it("returns null for file with no scrollback entries", async () => {
		const filePath = join(tmpDir, "no-sb.jsonl");
		await writeFile(filePath, makeSnapshot(1000) + "\n");

		const dump = await readScrollbackDump(filePath);
		expect(dump).toBeNull();
	});
});
