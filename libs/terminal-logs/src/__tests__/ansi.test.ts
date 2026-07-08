import { describe, expect, it } from "vitest";
import { runsToAnsi, snapshotToAnsi, stripRuns } from "../ansi.js";
import type { AttributeRun, GridSnapshot, ScrollbackDump } from "../schema.js";

const ESC = "\x1b";
const CSI = `${ESC}[`;
const RESET = `${CSI}0m`;

// ---------------------------------------------------------------------------
// stripRuns
// ---------------------------------------------------------------------------

describe("stripRuns", () => {
	it("extracts plain text from a single run", () => {
		const runs: AttributeRun[] = [
			{ t: "hello world", fg: "default", bg: "default", a: 0 },
		];
		expect(stripRuns(runs)).toBe("hello world");
	});

	it("concatenates multiple runs", () => {
		const runs: AttributeRun[] = [
			{ t: "hello ", fg: "default", bg: "default", a: 0 },
			{ t: "world", fg: [0, 255, 0], bg: "default", a: 1 },
		];
		expect(stripRuns(runs)).toBe("hello world");
	});

	it("expands trailing space repeat counts", () => {
		// `r` = number of trailing spaces stripped during compression
		// Rust: "hello     " → {t: "hello", r: 5} (5 trailing spaces)
		const runs: AttributeRun[] = [
			{ t: "hello", fg: "default", bg: "default", a: 0, r: 5 },
		];
		expect(stripRuns(runs)).toBe("hello     ");
	});

	it("handles empty runs array", () => {
		expect(stripRuns([])).toBe("");
	});

	it("handles mixed repeated and normal runs", () => {
		// Rust compresses all-space runs: 108 spaces → {t: " ", r: 107}
		// Reconstruction: " " + 107 spaces = 108 total
		const runs: AttributeRun[] = [
			{ t: "$ ", fg: "default", bg: "default", a: 0 },
			{ t: "git status", fg: "default", bg: "default", a: 0 },
			{ t: " ", fg: "default", bg: "default", a: 0, r: 107 },
		];
		expect(stripRuns(runs)).toBe("$ git status" + " ".repeat(108));
	});
});

// ---------------------------------------------------------------------------
// runsToAnsi
// ---------------------------------------------------------------------------

describe("runsToAnsi", () => {
	it("returns empty string for empty runs", () => {
		expect(runsToAnsi([])).toBe("");
	});

	it("wraps default-styled text with reset", () => {
		const runs: AttributeRun[] = [
			{ t: "hello", fg: "default", bg: "default", a: 0 },
		];
		const result = runsToAnsi(runs);
		// Default run: RESET + text + final RESET
		expect(result).toBe(`${RESET}hello${RESET}`);
	});

	it("applies bold attribute (SGR 1)", () => {
		const runs: AttributeRun[] = [
			{ t: "bold", fg: "default", bg: "default", a: 1 },
		];
		const result = runsToAnsi(runs);
		// a=1 → bold. SGR: 0 (reset) + 1 (bold) + 39 (default fg) + 49 (default bg)
		expect(result).toContain(`${CSI}0;1;39;49m`);
		expect(result).toContain("bold");
	});

	it("applies italic attribute (SGR 3)", () => {
		const runs: AttributeRun[] = [
			{ t: "italic", fg: "default", bg: "default", a: 2 },
		];
		const result = runsToAnsi(runs);
		expect(result).toContain(`${CSI}0;3;39;49m`);
	});

	it("applies combined bold+italic (SGR 1;3)", () => {
		const runs: AttributeRun[] = [
			{ t: "bold+italic", fg: "default", bg: "default", a: 3 },
		];
		const result = runsToAnsi(runs);
		expect(result).toContain(`${CSI}0;1;3;39;49m`);
	});

	it("applies indexed foreground color", () => {
		const runs: AttributeRun[] = [
			{ t: "red", fg: 196, bg: "default", a: 0 },
		];
		const result = runsToAnsi(runs);
		// fg=196 → 38;5;196
		expect(result).toContain("38;5;196");
	});

	it("applies RGB foreground color", () => {
		const runs: AttributeRun[] = [
			{ t: "green", fg: [0, 255, 0], bg: "default", a: 0 },
		];
		const result = runsToAnsi(runs);
		expect(result).toContain("38;2;0;255;0");
	});

	it("applies indexed background color", () => {
		const runs: AttributeRun[] = [
			{ t: "bgred", fg: "default", bg: 196, a: 0 },
		];
		const result = runsToAnsi(runs);
		expect(result).toContain("48;5;196");
	});

	it("applies RGB background color", () => {
		const runs: AttributeRun[] = [
			{ t: "bg", fg: "default", bg: [50, 50, 50], a: 0 },
		];
		const result = runsToAnsi(runs);
		expect(result).toContain("48;2;50;50;50");
	});

	it("expands trailing space repeat in styled runs", () => {
		// {t: " ", r: 3} → " " + 3 spaces = 4 spaces total
		const runs: AttributeRun[] = [
			{ t: " ", fg: "default", bg: 196, a: 0, r: 3 },
		];
		const result = runsToAnsi(runs);
		// Should contain 4 spaces total (1 in text + 3 repeated)
		expect(result).toContain("    ");
	});

	it("ends with a reset", () => {
		const runs: AttributeRun[] = [
			{ t: "x", fg: 196, bg: "default", a: 1 },
		];
		const result = runsToAnsi(runs);
		expect(result.endsWith(RESET)).toBe(true);
	});

	it("handles multiple runs with different styles", () => {
		const runs: AttributeRun[] = [
			{ t: "$ ", fg: "default", bg: "default", a: 0 },
			{ t: "git", fg: [0, 255, 0], bg: "default", a: 1 },
			{ t: " status", fg: "default", bg: "default", a: 0 },
		];
		const result = runsToAnsi(runs);
		// Should contain all text segments
		expect(result).toContain("$ ");
		expect(result).toContain("git");
		expect(result).toContain(" status");
		// Should contain green color for "git"
		expect(result).toContain("38;2;0;255;0");
	});
});

// ---------------------------------------------------------------------------
// snapshotToAnsi
// ---------------------------------------------------------------------------

describe("snapshotToAnsi", () => {
	it("converts a single-row snapshot", () => {
		const snapshot = {
			v: 1 as const,
			type: "snapshot" as const,
			ts: 1709395200.123,
			trigger: "prompt" as const,
			cols: 80,
			rows: 24,
			cursor: { col: 0, row: 0 },
			cwd: "/tmp",
			exit_code: 0,
			grid: [
				{
					row: 0,
					runs: [
						{ t: "hello", fg: "default" as const, bg: "default" as const, a: 0 },
					],
				},
			],
			sb_lines: 0,
			sb_hash: "",
		} satisfies GridSnapshot;

		const result = snapshotToAnsi(snapshot);
		expect(result).toContain("hello");
	});

	it("joins multiple rows with newlines", () => {
		const snapshot = {
			v: 1 as const,
			type: "snapshot" as const,
			ts: 1709395200.123,
			trigger: "manual" as const,
			cols: 80,
			rows: 24,
			cursor: { col: 0, row: 0 },
			cwd: "/tmp",
			exit_code: null,
			grid: [
				{
					row: 0,
					runs: [
						{ t: "line1", fg: "default" as const, bg: "default" as const, a: 0 },
					],
				},
				{
					row: 1,
					runs: [
						{ t: "line2", fg: "default" as const, bg: "default" as const, a: 0 },
					],
				},
			],
			sb_lines: 0,
			sb_hash: "",
		} satisfies GridSnapshot;

		const result = snapshotToAnsi(snapshot);
		// Filter empty trailing elements from split
		const lines = result.split("\n").filter((l) => l.length > 0);
		expect(lines.length).toBeGreaterThanOrEqual(2);
		expect(lines[0]).toContain("line1");
		expect(lines[1]).toContain("line2");
	});

	it("joins wrapped rows without newline", () => {
		const snapshot = {
			v: 1 as const,
			type: "snapshot" as const,
			ts: 1709395200.0,
			trigger: "manual" as const,
			cols: 3,
			rows: 3,
			cursor: { col: 0, row: 0 },
			cwd: "/tmp",
			exit_code: null,
			grid: [
				{
					row: 0,
					runs: [
						{ t: "AAA", fg: "default" as const, bg: "default" as const, a: 0 },
					],
					wrapped: true,
				},
				{
					row: 1,
					runs: [
						{ t: "BBB", fg: "default" as const, bg: "default" as const, a: 0 },
					],
					wrapped: false,
				},
			],
			sb_lines: 0,
			sb_hash: "",
		} satisfies GridSnapshot;

		const result = snapshotToAnsi(snapshot);
		// Strip ANSI escapes for content check
		const plain = result.replace(/\x1b\[[0-9;]*m/g, "");
		// Wrapped rows should be joined — "AAABBB" on one logical line
		expect(plain).toContain("AAABBB");
	});

	it("includes scrollback with wrapped lines", () => {
		const snapshot = {
			v: 1 as const,
			type: "snapshot" as const,
			ts: 1709395200.0,
			trigger: "manual" as const,
			cols: 4,
			rows: 2,
			cursor: { col: 0, row: 0 },
			cwd: "/tmp",
			exit_code: null,
			grid: [
				{
					row: 0,
					runs: [
						{ t: "grid", fg: "default" as const, bg: "default" as const, a: 0 },
					],
				},
			],
			sb_lines: 3,
			sb_hash: "abc",
		} satisfies GridSnapshot;

		const scrollback: ScrollbackDump = {
			v: 1,
			type: "scrollback",
			ts: 1709395200.0,
			lines: [
				{
					runs: [{ t: "XXXX", fg: "default", bg: "default", a: 0 }],
					wrapped: true,
				},
				{
					runs: [{ t: "YYYY", fg: "default", bg: "default", a: 0 }],
					wrapped: false,
				},
			],
			hash: "abc",
		};

		const result = snapshotToAnsi(snapshot, scrollback);
		const plain = result.replace(/\x1b\[[0-9;]*m/g, "");
		// Scrollback wrapped lines should join
		expect(plain).toContain("XXXXYYYY");
		// Grid content should follow
		expect(plain).toContain("grid");
	});

	it("preserves colors in multi-row snapshot", () => {
		const snapshot = {
			v: 1 as const,
			type: "snapshot" as const,
			ts: 1709395200.0,
			trigger: "periodic" as const,
			cols: 120,
			rows: 36,
			cursor: { col: 0, row: 0 },
			cwd: "/project",
			exit_code: 0,
			grid: [
				{
					row: 0,
					runs: [
						{ t: "On branch ", fg: "default" as const, bg: "default" as const, a: 0 },
						{ t: "main", fg: [0, 255, 0] as [number, number, number], bg: "default" as const, a: 1 },
					],
				},
			],
			sb_lines: 100,
			sb_hash: "abc",
		} satisfies GridSnapshot;

		const result = snapshotToAnsi(snapshot);
		expect(result).toContain("On branch ");
		expect(result).toContain("main");
		expect(result).toContain("38;2;0;255;0"); // green RGB
	});
});
