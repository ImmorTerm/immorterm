import { z } from "zod";

// ---------------------------------------------------------------------------
// Color
// ---------------------------------------------------------------------------
// A color in log files is one of:
//   "default"        - terminal default fg/bg
//   number (0-255)   - indexed color
//   [r, g, b]        - 24-bit true color
// ---------------------------------------------------------------------------

export const ColorSchema = z.union([
	z.literal("default"),
	z.number().int().min(0).max(255),
	z.tuple([
		z.number().int().min(0).max(255),
		z.number().int().min(0).max(255),
		z.number().int().min(0).max(255),
	]),
]);

export type Color = z.infer<typeof ColorSchema>;

// ---------------------------------------------------------------------------
// AttributeRun
// ---------------------------------------------------------------------------
// A run of text sharing identical styling attributes.
//
// Attribute bitfield `a`:
//   1  = bold
//   2  = italic
//   4  = underline
//   8  = strikethrough
//   16 = dim
//   32 = inverse
//   64 = blink
//
// Optional `r` is the number of trailing spaces stripped during compression.
// To reconstruct: append `r` space characters after the text `t`.
// ---------------------------------------------------------------------------

export const AttributeRunSchema = z.object({
	t: z.string(),
	fg: ColorSchema,
	bg: ColorSchema,
	a: z.number().int(),
	r: z.number().int().min(1).optional(),
});

export type AttributeRun = z.infer<typeof AttributeRunSchema>;

// ---------------------------------------------------------------------------
// RowRuns
// ---------------------------------------------------------------------------

export const RowRunsSchema = z.object({
	row: z.number().int(),
	runs: z.array(AttributeRunSchema),
	/** Whether this row soft-wraps to the next row (omitted when false). */
	wrapped: z.boolean().optional().default(false),
});

export type RowRuns = z.infer<typeof RowRunsSchema>;

// ---------------------------------------------------------------------------
// GridSnapshot  (type = "snapshot")
// ---------------------------------------------------------------------------

export const GridSnapshotSchema = z.object({
	v: z.literal(1),
	type: z.literal("snapshot"),
	ts: z.number(),
	trigger: z.enum(["prompt", "periodic", "shutdown", "manual"]),
	cols: z.number().int().positive(),
	rows: z.number().int().positive(),
	cursor: z.object({
		col: z.number().int().min(0),
		row: z.number().int().min(0),
	}),
	cwd: z.string(),
	exit_code: z.number().int().nullable(),
	grid: z.array(RowRunsSchema),
	sb_lines: z.number().int().min(0),
	sb_hash: z.string(),
});

export type GridSnapshot = z.infer<typeof GridSnapshotSchema>;

// ---------------------------------------------------------------------------
// ScrollbackDump  (type = "scrollback")
// ---------------------------------------------------------------------------

export const ScrollbackDumpSchema = z.object({
	v: z.literal(1),
	type: z.literal("scrollback"),
	ts: z.number(),
	lines: z.array(
		z.object({
			runs: z.array(AttributeRunSchema),
			/** Whether this line soft-wraps to the next line (omitted when false). */
			wrapped: z.boolean().optional().default(false),
		}),
	),
	hash: z.string(),
});

export type ScrollbackDump = z.infer<typeof ScrollbackDumpSchema>;

// ---------------------------------------------------------------------------
// Asciicast v2  (header + events)
// ---------------------------------------------------------------------------

export const AsciicastHeaderSchema = z.object({
	version: z.literal(2),
	width: z.number().int().positive(),
	height: z.number().int().positive(),
	timestamp: z.number(),
	env: z.record(z.string()).optional(),
});

export type AsciicastHeader = z.infer<typeof AsciicastHeaderSchema>;

export const AsciicastEventSchema = z.tuple([
	z.number(), // time_offset in seconds
	z.enum(["o", "i", "r"]), // output, input, resize
	z.string(), // data
]);

export type AsciicastEvent = z.infer<typeof AsciicastEventSchema>;

// ---------------------------------------------------------------------------
// AI Conversation Events  (discriminated union on `event` field)
// ---------------------------------------------------------------------------

const aiEventBase = {
	v: z.literal(1),
	ts: z.number(),
};

export const AiDetectedEventSchema = z.object({
	...aiEventBase,
	event: z.literal("ai_detected"),
	tool: z.string(),
	pid: z.number().int(),
	transcript_path: z.string().optional(),
});

export type AiDetectedEvent = z.infer<typeof AiDetectedEventSchema>;

export const HtmlBlockSchema = z.object({
	/** Zero-based index of this block within the turn. */
	index: z.number().int().min(0),
	/** The raw HTML content between the <<html>> markers. */
	html: z.string(),
});

export type HtmlBlock = z.infer<typeof HtmlBlockSchema>;

export const AiTurnEventSchema = z.object({
	...aiEventBase,
	event: z.literal("turn"),
	role: z.enum(["user", "assistant"]),
	/** Cleaned content with <<html>> blocks stripped. */
	content: z.string(),
	/** Original content with <<html>> blocks preserved (only when HTML was present). */
	content_raw: z.string().optional(),
	/** Extracted HTML blocks for interactive replay (only when HTML was present). */
	html_blocks: z.array(HtmlBlockSchema).optional(),
	tools_visible: z.array(z.string()).optional(),
});

export type AiTurnEvent = z.infer<typeof AiTurnEventSchema>;

export const AiExitedEventSchema = z.object({
	...aiEventBase,
	event: z.literal("ai_exited"),
	tool: z.string(),
	duration_s: z.number(),
	cost_usd: z.number().optional(),
});

export type AiExitedEvent = z.infer<typeof AiExitedEventSchema>;

export const AiConversationEventSchema = z.discriminatedUnion("event", [
	AiDetectedEventSchema,
	AiTurnEventSchema,
	AiExitedEventSchema,
]);

export type AiConversationEvent = z.infer<typeof AiConversationEventSchema>;
