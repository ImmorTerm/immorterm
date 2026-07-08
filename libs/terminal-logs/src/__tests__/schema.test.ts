import { describe, expect, it } from "vitest";
import {
	AiConversationEventSchema,
	AiDetectedEventSchema,
	AiExitedEventSchema,
	AiTurnEventSchema,
	AsciicastEventSchema,
	AsciicastHeaderSchema,
	AttributeRunSchema,
	ColorSchema,
	GridSnapshotSchema,
	RowRunsSchema,
	ScrollbackDumpSchema,
} from "../schema.js";

// ---------------------------------------------------------------------------
// Color
// ---------------------------------------------------------------------------

describe("ColorSchema", () => {
	it("accepts 'default'", () => {
		expect(ColorSchema.parse("default")).toBe("default");
	});

	it("accepts indexed color (0-255)", () => {
		expect(ColorSchema.parse(0)).toBe(0);
		expect(ColorSchema.parse(196)).toBe(196);
		expect(ColorSchema.parse(255)).toBe(255);
	});

	it("accepts RGB tuple", () => {
		expect(ColorSchema.parse([0, 128, 255])).toEqual([0, 128, 255]);
	});

	it("rejects invalid string", () => {
		expect(ColorSchema.safeParse("red").success).toBe(false);
	});

	it("rejects out-of-range index", () => {
		expect(ColorSchema.safeParse(256).success).toBe(false);
		expect(ColorSchema.safeParse(-1).success).toBe(false);
	});

	it("rejects RGB with wrong length", () => {
		expect(ColorSchema.safeParse([0, 128]).success).toBe(false);
		expect(ColorSchema.safeParse([0, 128, 255, 0]).success).toBe(false);
	});

	it("rejects RGB with out-of-range values", () => {
		expect(ColorSchema.safeParse([0, 0, 256]).success).toBe(false);
	});
});

// ---------------------------------------------------------------------------
// AttributeRun
// ---------------------------------------------------------------------------

describe("AttributeRunSchema", () => {
	it("parses a basic run", () => {
		const run = { t: "hello", fg: "default", bg: "default", a: 0 };
		expect(AttributeRunSchema.parse(run)).toEqual(run);
	});

	it("parses a run with styling", () => {
		const run = { t: "bold text", fg: [0, 255, 0], bg: 196, a: 1 };
		expect(AttributeRunSchema.parse(run)).toEqual(run);
	});

	it("parses a run with repeat count", () => {
		const run = { t: " ", fg: "default", bg: "default", a: 0, r: 80 };
		const parsed = AttributeRunSchema.parse(run);
		expect(parsed.r).toBe(80);
	});

	it("rejects repeat count < 1", () => {
		const run = { t: " ", fg: "default", bg: "default", a: 0, r: 0 };
		expect(AttributeRunSchema.safeParse(run).success).toBe(false);
	});
});

// ---------------------------------------------------------------------------
// GridSnapshot
// ---------------------------------------------------------------------------

describe("GridSnapshotSchema", () => {
	const validSnapshot = {
		v: 1,
		type: "snapshot",
		ts: 1709395200.123,
		trigger: "prompt",
		cols: 120,
		rows: 36,
		cursor: { col: 4, row: 23 },
		cwd: "/Users/example/project",
		exit_code: 0,
		grid: [
			{
				row: 0,
				runs: [
					{ t: "$ git status", fg: "default", bg: "default", a: 0 },
					{ t: " ", fg: "default", bg: "default", a: 0, r: 108 },
				],
			},
			{
				row: 1,
				runs: [
					{ t: "On branch ", fg: "default", bg: "default", a: 0 },
					{ t: "main", fg: [0, 255, 0], bg: "default", a: 1 },
				],
			},
		],
		sb_lines: 1542,
		sb_hash: "abc123",
	};

	it("parses a valid snapshot", () => {
		const parsed = GridSnapshotSchema.parse(validSnapshot);
		expect(parsed.trigger).toBe("prompt");
		expect(parsed.grid).toHaveLength(2);
		expect(parsed.grid[0]!.runs[0]!.t).toBe("$ git status");
	});

	it("accepts all trigger types", () => {
		for (const trigger of ["prompt", "periodic", "shutdown", "manual"]) {
			const snapshot = { ...validSnapshot, trigger };
			expect(GridSnapshotSchema.safeParse(snapshot).success).toBe(true);
		}
	});

	it("accepts null exit_code", () => {
		const snapshot = { ...validSnapshot, exit_code: null };
		expect(GridSnapshotSchema.parse(snapshot).exit_code).toBeNull();
	});

	it("rejects wrong version", () => {
		const bad = { ...validSnapshot, v: 2 };
		expect(GridSnapshotSchema.safeParse(bad).success).toBe(false);
	});

	it("rejects wrong type", () => {
		const bad = { ...validSnapshot, type: "scrollback" };
		expect(GridSnapshotSchema.safeParse(bad).success).toBe(false);
	});

	it("rejects negative cols/rows", () => {
		expect(GridSnapshotSchema.safeParse({ ...validSnapshot, cols: 0 }).success).toBe(false);
		expect(GridSnapshotSchema.safeParse({ ...validSnapshot, rows: -1 }).success).toBe(false);
	});
});

// ---------------------------------------------------------------------------
// ScrollbackDump
// ---------------------------------------------------------------------------

describe("ScrollbackDumpSchema", () => {
	const validDump = {
		v: 1,
		type: "scrollback",
		ts: 1709395200.0,
		lines: [
			{
				runs: [
					{ t: "some scrollback line", fg: "default", bg: "default", a: 0 },
				],
			},
			{
				runs: [
					{ t: "another line", fg: 2, bg: "default", a: 0 },
				],
			},
		],
		hash: "abc123",
	};

	it("parses a valid scrollback dump", () => {
		const parsed = ScrollbackDumpSchema.parse(validDump);
		expect(parsed.lines).toHaveLength(2);
		expect(parsed.hash).toBe("abc123");
	});

	it("rejects wrong type", () => {
		const bad = { ...validDump, type: "snapshot" };
		expect(ScrollbackDumpSchema.safeParse(bad).success).toBe(false);
	});
});

// ---------------------------------------------------------------------------
// Asciicast
// ---------------------------------------------------------------------------

describe("AsciicastHeaderSchema", () => {
	it("parses a valid header", () => {
		const header = { version: 2, width: 120, height: 36, timestamp: 1709395200 };
		expect(AsciicastHeaderSchema.parse(header).width).toBe(120);
	});

	it("accepts optional env", () => {
		const header = {
			version: 2,
			width: 80,
			height: 24,
			timestamp: 1709395200,
			env: { TERM: "xterm-256color", SHELL: "/bin/zsh" },
		};
		expect(AsciicastHeaderSchema.parse(header).env?.TERM).toBe("xterm-256color");
	});

	it("rejects wrong version", () => {
		const bad = { version: 1, width: 80, height: 24, timestamp: 0 };
		expect(AsciicastHeaderSchema.safeParse(bad).success).toBe(false);
	});
});

describe("AsciicastEventSchema", () => {
	it("parses output event", () => {
		const event = [0.248, "o", "$ "];
		expect(AsciicastEventSchema.parse(event)).toEqual(event);
	});

	it("parses input event", () => {
		const event = [1.5, "i", "ls\r\n"];
		expect(AsciicastEventSchema.parse(event)).toEqual(event);
	});

	it("parses resize event", () => {
		const event = [12.5, "r", "120x36"];
		expect(AsciicastEventSchema.parse(event)).toEqual(event);
	});

	it("rejects unknown event type", () => {
		expect(AsciicastEventSchema.safeParse([0, "x", "data"]).success).toBe(false);
	});
});

// ---------------------------------------------------------------------------
// AI Conversation Events
// ---------------------------------------------------------------------------

describe("AiConversationEventSchema", () => {
	it("parses ai_detected event", () => {
		const event = {
			v: 1,
			ts: 1709395200.5,
			event: "ai_detected",
			tool: "claude",
			pid: 12345,
			transcript_path: "/path/to/transcript.jsonl",
		};
		const parsed = AiDetectedEventSchema.parse(event);
		expect(parsed.tool).toBe("claude");
		expect(parsed.pid).toBe(12345);
	});

	it("parses turn event", () => {
		const event = {
			v: 1,
			ts: 1709395201.0,
			event: "turn",
			role: "user",
			content: "Fix the login bug",
		};
		const parsed = AiTurnEventSchema.parse(event);
		expect(parsed.role).toBe("user");
		expect(parsed.content).toBe("Fix the login bug");
	});

	it("parses assistant turn with tools_visible", () => {
		const event = {
			v: 1,
			ts: 1709395215.0,
			event: "turn",
			role: "assistant",
			content: "I'll fix the auth...",
			tools_visible: ["Read auth.ts", "Edit auth.ts"],
		};
		const parsed = AiTurnEventSchema.parse(event);
		expect(parsed.tools_visible).toEqual(["Read auth.ts", "Edit auth.ts"]);
	});

	it("parses ai_exited event", () => {
		const event = {
			v: 1,
			ts: 1709395300.0,
			event: "ai_exited",
			tool: "claude",
			duration_s: 99.5,
			cost_usd: 0.42,
		};
		const parsed = AiExitedEventSchema.parse(event);
		expect(parsed.duration_s).toBe(99.5);
		expect(parsed.cost_usd).toBe(0.42);
	});

	it("discriminated union resolves correctly", () => {
		const events = [
			{ v: 1, ts: 1, event: "ai_detected", tool: "aider", pid: 1 },
			{ v: 1, ts: 2, event: "turn", role: "user", content: "hello" },
			{ v: 1, ts: 3, event: "ai_exited", tool: "aider", duration_s: 10 },
		];
		for (const event of events) {
			expect(AiConversationEventSchema.safeParse(event).success).toBe(true);
		}
	});

	it("rejects unknown event type", () => {
		const bad = { v: 1, ts: 1, event: "unknown", tool: "x", pid: 1 };
		expect(AiConversationEventSchema.safeParse(bad).success).toBe(false);
	});
});
