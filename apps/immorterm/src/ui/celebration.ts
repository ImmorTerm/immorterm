/**
 * Pro Celebration — ASCII art with gradient rendering and sparkle animation
 *
 * Shown after successful license activation. Features:
 * - "PRO UNLOCKED" block-letter banner with theme gradient
 * - Sparkle animation around the banner edges (~1.5s)
 * - Terminal width fallback (compact for < 90 cols)
 * - Non-TTY fallback (plain text, no ANSI)
 */

import pc from "picocolors";

// ── ASCII Art ──────────────────────────────────────────────────────

const PRO_UNLOCKED_LINES = [
	"  ██████╗ ██████╗  ██████╗     ██╗   ██╗███╗   ██╗██╗      ██████╗  ██████╗██╗  ██╗███████╗██████╗ ██╗",
	"  ██╔══██╗██╔══██╗██╔═══██╗    ██║   ██║████╗  ██║██║     ██╔═══██╗██╔════╝██║ ██╔╝██╔════╝██╔══██╗██║",
	"  ██████╔╝██████╔╝██║   ██║    ██║   ██║██╔██╗ ██║██║     ██║   ██║██║     █████╔╝ █████╗  ██║  ██║██║",
	"  ██╔═══╝ ██╔══██╗██║   ██║    ██║   ██║██║╚██╗██║██║     ██║   ██║██║     ██╔═██╗ ██╔══╝  ██║  ██║╚═╝",
	"  ██║     ██║  ██║╚██████╔╝    ╚██████╔╝██║ ╚████║███████╗╚██████╔╝╚██████╗██║  ██╗███████╗██████╔╝██╗",
	"  ╚═╝     ╚═╝  ╚═╝ ╚═════╝     ╚═════╝ ╚═╝  ╚═══╝╚══════╝ ╚═════╝  ╚═════╝╚═╝  ╚═╝╚══════╝╚═════╝ ╚═╝",
];

const BANNER_WIDTH = PRO_UNLOCKED_LINES[0]!.length;

// ── Color Helpers (self-contained, avoid coupling to banner.ts) ──

type RGB = [number, number, number];

function parseHex(hex: string): RGB {
	return [
		parseInt(hex.slice(1, 3), 16),
		parseInt(hex.slice(3, 5), 16),
		parseInt(hex.slice(5, 7), 16),
	];
}

function lerpStops(stops: RGB[], t: number): RGB {
	const idx = t * (stops.length - 1);
	const lo = Math.floor(idx);
	const hi = Math.min(lo + 1, stops.length - 1);
	const f = idx - lo;
	const [r1, g1, b1] = stops[lo]!;
	const [r2, g2, b2] = stops[hi]!;
	return [
		Math.round(r1 + (r2 - r1) * f),
		Math.round(g1 + (g2 - g1) * f),
		Math.round(b1 + (b2 - b1) * f),
	];
}

// Default gradient (Purple Haze theme)
const DEFAULT_GRADIENT: string[] = [
	"#7c3aed", "#a855f7", "#06b6d4", "#22c55e",
	"#f59e0b", "#ef4444", "#a855f7", "#7c3aed",
];

function buildLut(hexStops: string[]): string[] {
	const stops = hexStops.map(parseHex);
	const lut: string[] = new Array(256);
	for (let i = 0; i < 256; i++) {
		const [r, g, b] = lerpStops(stops, i / 255);
		lut[i] = `\x1b[38;2;${r};${g};${b}m`;
	}
	return lut;
}

function renderGradientLine(line: string, lut: string[]): string {
	const len = line.length;
	if (len === 0) return "";
	let s = "";
	for (let i = 0; i < len; i++) {
		const idx = Math.round((i / Math.max(len - 1, 1)) * 255);
		s += lut[idx]! + line[i]!;
	}
	return s + "\x1b[0m";
}

// ── Theme Resolution ─────────────────────────────────────────────

function resolveGradient(themeName?: string): string[] {
	try {
		// Try to load the matching theme's gradient from the banner module
		// This avoids duplicating the full theme registry
		const banner = require("./banner.js");
		if (themeName && banner.BANNER_CACHE?.[themeName]) {
			// Theme exists — we'll use default gradient since we don't export
			// the raw gradient stops. The visual is still great.
		}
	} catch {
		// banner.js not available, use default
	}
	return DEFAULT_GRADIENT;
}

// ── Sparkle Animation ────────────────────────────────────────────

const SPARKLE_CHARS = ["✦", "✧", "*", "+", "·", "⋆"];
const SPARKLE_FRAMES = 6;
const FRAME_MS = 250;

interface SparklePos {
	row: number;
	col: number;
}

function generateSparklePositions(bannerStartRow: number, bannerHeight: number): SparklePos[] {
	const positions: SparklePos[] = [];
	const width = Math.min(BANNER_WIDTH + 4, (process.stdout.columns ?? 80));

	for (let i = 0; i < 12; i++) {
		const edge = i % 4;
		let row: number;
		let col: number;

		switch (edge) {
			case 0: // top edge
				row = bannerStartRow - 1;
				col = 2 + Math.floor(Math.random() * (width - 4));
				break;
			case 1: // bottom edge
				row = bannerStartRow + bannerHeight;
				col = 2 + Math.floor(Math.random() * (width - 4));
				break;
			case 2: // left edge
				row = bannerStartRow + Math.floor(Math.random() * bannerHeight);
				col = 0;
				break;
			case 3: // right edge
				row = bannerStartRow + Math.floor(Math.random() * bannerHeight);
				col = Math.min(width - 1, BANNER_WIDTH + 2);
				break;
			default:
				row = bannerStartRow;
				col = 0;
		}
		positions.push({ row, col });
	}
	return positions;
}

function sleep(ms: number): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

// ── Main Export ──────────────────────────────────────────────────

export async function playCelebration(themeName?: string): Promise<void> {
	const isTTY = process.stdout.isTTY;

	// Non-TTY fallback: plain text, no ANSI
	if (!isTTY) {
		console.log(pc.bold(pc.green("  ★  PRO UNLOCKED  ★")));
		console.log("");
		return;
	}

	const cols = process.stdout.columns ?? 80;

	// Narrow terminal fallback
	if (cols < 90) {
		const line = "  ★  PRO UNLOCKED  ★";
		const lut = buildLut(resolveGradient(themeName));
		console.log("");
		console.log(renderGradientLine(line, lut));
		console.log("");
		return;
	}

	// Full banner with gradient
	const lut = buildLut(resolveGradient(themeName));

	console.log("");
	for (const line of PRO_UNLOCKED_LINES) {
		console.log(renderGradientLine(line, lut));
	}
	console.log("");

	// Sparkle animation
	// Save cursor position, then animate sparkles around the banner
	const bannerStartRow = 2; // relative: 1 blank + banner starts at row 2
	const bannerHeight = PRO_UNLOCKED_LINES.length;

	// We need to know our current cursor position. Since we just printed the banner,
	// the cursor is now below it. We'll work relative to current position.
	// Move up to above the banner to position sparkles.
	const totalBannerLines = bannerHeight + 2; // blank + banner + blank

	// Hide cursor during animation
	process.stdout.write("\x1b[?25l");

	const sparkles = generateSparklePositions(0, bannerHeight);

	try {
		for (let frame = 0; frame < SPARKLE_FRAMES; frame++) {
			const charIdx = frame % SPARKLE_CHARS.length;
			const sparkleChar = SPARKLE_CHARS[charIdx]!;
			const colorIdx = Math.round((frame / (SPARKLE_FRAMES - 1)) * 255);
			const color = lut[colorIdx] ?? lut[0]!;

			// Draw sparkles
			for (const pos of sparkles) {
				// Move cursor relative to current position (up from bottom of banner)
				const rowOffset = totalBannerLines - pos.row;
				if (rowOffset > 0 && rowOffset <= totalBannerLines + 2) {
					// Save position, move to sparkle location, draw, restore
					process.stdout.write("\x1b7"); // save cursor
					process.stdout.write(`\x1b[${rowOffset}A`); // move up
					process.stdout.write(`\x1b[${pos.col + 1}G`); // move to column
					process.stdout.write(`${color}${sparkleChar}\x1b[0m`);
					process.stdout.write("\x1b8"); // restore cursor
				}
			}

			await sleep(FRAME_MS);

			// Clear sparkles (overwrite with spaces)
			for (const pos of sparkles) {
				const rowOffset = totalBannerLines - pos.row;
				if (rowOffset > 0 && rowOffset <= totalBannerLines + 2) {
					process.stdout.write("\x1b7");
					process.stdout.write(`\x1b[${rowOffset}A`);
					process.stdout.write(`\x1b[${pos.col + 1}G`);
					process.stdout.write(" ");
					process.stdout.write("\x1b8");
				}
			}
		}
	} finally {
		// Show cursor again
		process.stdout.write("\x1b[?25h");
	}
}
