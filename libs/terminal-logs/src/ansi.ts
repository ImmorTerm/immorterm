import type { AttributeRun, Color, GridSnapshot, ScrollbackDump } from "./schema.js";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const ESC = "\x1b";
const CSI = `${ESC}[`;
const SGR_RESET = `${CSI}0m`;

// Attribute bitfield → SGR parameter code mapping
// Bit: 1=bold(SGR 1), 2=italic(SGR 3), 4=underline(SGR 4),
//      8=strikethrough(SGR 9), 16=dim(SGR 2), 32=inverse(SGR 7), 64=blink(SGR 5)
const ATTR_SGR_MAP: ReadonlyArray<readonly [number, number]> = [
	[1, 1], // bold
	[2, 3], // italic
	[4, 4], // underline
	[8, 9], // strikethrough
	[16, 2], // dim
	[32, 7], // inverse
	[64, 5], // blink
] as const;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

function colorToSgr(color: Color, isForeground: boolean): string {
	const base = isForeground ? 38 : 48;
	const defaultCode = isForeground ? 39 : 49;

	if (color === "default") {
		return String(defaultCode);
	}
	if (typeof color === "number") {
		return `${base};5;${color}`;
	}
	// RGB tuple
	return `${base};2;${color[0]};${color[1]};${color[2]}`;
}

function attrsToSgrParams(attrs: number): string[] {
	const params: string[] = [];
	for (const [bit, sgr] of ATTR_SGR_MAP) {
		if (attrs & bit) {
			params.push(String(sgr));
		}
	}
	return params;
}

function expandText(run: AttributeRun): string {
	// `r` is the number of trailing spaces stripped during compression,
	// NOT a text repeat count. Append r spaces after the text content.
	if (run.r !== undefined && run.r > 0) {
		return run.t + " ".repeat(run.r);
	}
	return run.t;
}

// ---------------------------------------------------------------------------
// runsToAnsi
// ---------------------------------------------------------------------------
// Convert an array of AttributeRuns to a string with ANSI escape sequences.
// Each run emits a full SGR sequence (reset + set) to avoid state leakage.
// ---------------------------------------------------------------------------

export function runsToAnsi(runs: AttributeRun[]): string {
	if (runs.length === 0) return "";

	const parts: string[] = [];

	for (const run of runs) {
		const text = expandText(run);
		if (text.length === 0) continue;

		// Check if this run is fully default (no styling needed)
		const isDefault = run.fg === "default" && run.bg === "default" && run.a === 0;

		if (isDefault) {
			parts.push(SGR_RESET);
			parts.push(text);
			continue;
		}

		// Build SGR parameter list
		const sgrParams: string[] = ["0"]; // always reset first
		sgrParams.push(...attrsToSgrParams(run.a));
		sgrParams.push(colorToSgr(run.fg, true));
		sgrParams.push(colorToSgr(run.bg, false));

		parts.push(`${CSI}${sgrParams.join(";")}m`);
		parts.push(text);
	}

	// Final reset to leave terminal in clean state
	parts.push(SGR_RESET);

	return parts.join("");
}

// ---------------------------------------------------------------------------
// stripRuns
// ---------------------------------------------------------------------------
// Extract plain text from runs, expanding repeat counts. No ANSI escapes.
// ---------------------------------------------------------------------------

export function stripRuns(runs: AttributeRun[]): string {
	const parts: string[] = [];
	for (const run of runs) {
		parts.push(expandText(run));
	}
	return parts.join("");
}

// ---------------------------------------------------------------------------
// snapshotToAnsi
// ---------------------------------------------------------------------------
// Convert a full GridSnapshot (+ optional scrollback) to an ANSI string
// suitable for terminal restoration. Uses the `wrapped` flag to join
// soft-wrapped rows into logical lines — only emits `\n` at hard breaks.
// ---------------------------------------------------------------------------

export function snapshotToAnsi(
	snapshot: GridSnapshot,
	scrollback?: ScrollbackDump,
): string {
	const parts: string[] = [];

	// Scrollback first — join wrapped lines into logical lines
	if (scrollback) {
		for (const line of scrollback.lines) {
			parts.push(runsToAnsi(line.runs));
			if (!line.wrapped) {
				parts.push("\n");
			}
		}
	}

	// Grid rows — join wrapped rows, emit \n only at hard breaks
	let lastRow = 0;
	for (const rowRuns of snapshot.grid) {
		// Fill empty rows between last and current
		while (lastRow < rowRuns.row) {
			parts.push("\n");
			lastRow++;
		}
		parts.push(runsToAnsi(rowRuns.runs));
		// Only emit newline at hard breaks, not soft wraps
		if (!rowRuns.wrapped && rowRuns.row < snapshot.rows - 1) {
			parts.push("\n");
		}
		lastRow = rowRuns.row + 1;
	}

	return parts.join("");
}
