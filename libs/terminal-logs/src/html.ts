/**
 * HTML Rendering — Convert AttributeRun[] to styled HTML spans
 *
 * Used by the web dashboard to render terminal snapshots in the browser.
 * Maps ANSI colors and attributes to inline CSS styles.
 */

import type { AttributeRun, Color, GridSnapshot, RowRuns, ScrollbackDump } from "./schema.js";

// ---------------------------------------------------------------------------
// ANSI 16-color palette → CSS hex values
// ---------------------------------------------------------------------------

const ANSI_16_COLORS: readonly string[] = [
	"#000000", // 0 black
	"#cc0000", // 1 red
	"#4e9a06", // 2 green
	"#c4a000", // 3 yellow
	"#3465a4", // 4 blue
	"#75507b", // 5 magenta
	"#06989a", // 6 cyan
	"#d3d7cf", // 7 white
	"#555753", // 8 bright black
	"#ef2929", // 9 bright red
	"#8ae234", // 10 bright green
	"#fce94f", // 11 bright yellow
	"#729fcf", // 12 bright blue
	"#ad7fa8", // 13 bright magenta
	"#34e2e2", // 14 bright cyan
	"#eeeeec", // 15 bright white
] as const;

// ---------------------------------------------------------------------------
// 256-color palette generation (colors 16-231 are a 6x6x6 cube, 232-255 are grays)
// ---------------------------------------------------------------------------

function indexed256ToHex(index: number): string {
	if (index < 16) return ANSI_16_COLORS[index]!;

	if (index < 232) {
		// 6x6x6 color cube
		const i = index - 16;
		const r = Math.floor(i / 36);
		const g = Math.floor((i % 36) / 6);
		const b = i % 6;
		const toHex = (v: number) => (v === 0 ? 0 : 55 + v * 40);
		return `rgb(${toHex(r)},${toHex(g)},${toHex(b)})`;
	}

	// Grayscale ramp (232-255)
	const gray = 8 + (index - 232) * 10;
	return `rgb(${gray},${gray},${gray})`;
}

// ---------------------------------------------------------------------------
// Color → CSS value
// ---------------------------------------------------------------------------

function colorToCSS(color: Color, isForeground: boolean): string | null {
	if (color === "default") return null;
	if (typeof color === "number") return indexed256ToHex(color);
	return `rgb(${color[0]},${color[1]},${color[2]})`;
}

// ---------------------------------------------------------------------------
// Escape HTML entities
// ---------------------------------------------------------------------------

function escapeHtml(text: string): string {
	return text
		.replace(/&/g, "&amp;")
		.replace(/</g, "&lt;")
		.replace(/>/g, "&gt;")
		.replace(/"/g, "&quot;");
}

// ---------------------------------------------------------------------------
// runsToHtml — Convert AttributeRun[] to styled HTML spans
// ---------------------------------------------------------------------------

export function runsToHtml(runs: AttributeRun[]): string {
	if (runs.length === 0) return "";

	const parts: string[] = [];

	for (const run of runs) {
		// `r` is the number of trailing spaces stripped during compression,
		// NOT a text repeat count. Append r spaces after the text content.
		const text = run.r !== undefined && run.r > 0 ? run.t + " ".repeat(run.r) : run.t;
		if (text.length === 0) continue;

		const styles: string[] = [];

		// Foreground color
		const fg = colorToCSS(run.fg, true);
		if (fg) styles.push(`color:${fg}`);

		// Background color
		const bg = colorToCSS(run.bg, false);
		if (bg) styles.push(`background-color:${bg}`);

		// Attributes
		if (run.a & 1) styles.push("font-weight:bold");
		if (run.a & 2) styles.push("font-style:italic");
		if (run.a & 4) styles.push("text-decoration:underline");
		if (run.a & 8) styles.push("text-decoration:line-through");
		if (run.a & 16) styles.push("opacity:0.6"); // dim
		if (run.a & 64) styles.push("animation:blink 1s step-end infinite");

		// Handle combined text-decoration
		const decos: string[] = [];
		if (run.a & 4) decos.push("underline");
		if (run.a & 8) decos.push("line-through");
		if (decos.length > 0) {
			// Remove individual text-decoration entries and combine
			const filtered = styles.filter((s) => !s.startsWith("text-decoration:"));
			filtered.push(`text-decoration:${decos.join(" ")}`);
			styles.length = 0;
			styles.push(...filtered);
		}

		const escaped = escapeHtml(text);

		if (styles.length === 0) {
			parts.push(`<span>${escaped}</span>`);
		} else {
			parts.push(`<span style="${styles.join(";")}">${escaped}</span>`);
		}
	}

	return parts.join("");
}

// ---------------------------------------------------------------------------
// rowsToHtml — Convert RowRuns[] to HTML lines
// ---------------------------------------------------------------------------

export function rowsToHtml(rows: RowRuns[]): string {
	const lines: string[] = [];
	let currentLine = "";

	for (const row of rows) {
		currentLine += runsToHtml(row.runs);
		if (!row.wrapped) {
			lines.push(currentLine);
			currentLine = "";
		}
	}

	// Flush any remaining content
	if (currentLine) {
		lines.push(currentLine);
	}

	return lines.join("\n");
}

// ---------------------------------------------------------------------------
// snapshotToHtml — Full snapshot with terminal-styled wrapper
// ---------------------------------------------------------------------------

export function snapshotToHtml(
	snapshot: GridSnapshot,
	scrollback?: ScrollbackDump,
): string {
	const parts: string[] = [];

	// Scrollback lines
	if (scrollback) {
		let sbLine = "";
		for (const line of scrollback.lines) {
			sbLine += runsToHtml(line.runs);
			if (!line.wrapped) {
				parts.push(sbLine);
				sbLine = "";
			}
		}
		if (sbLine) parts.push(sbLine);
	}

	// Grid rows
	parts.push(rowsToHtml(snapshot.grid));

	const content = parts.join("\n");

	return [
		`<div class="terminal-snapshot" style="font-family:'JetBrains Mono',monospace;font-size:13px;line-height:1.4;background:#0a0a0a;color:#d3d7cf;padding:12px;border-radius:8px;overflow-x:auto;white-space:pre">`,
		content,
		`</div>`,
	].join("");
}
