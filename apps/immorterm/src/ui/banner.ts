/**
 * ImmorTerm CLI Banner — Themed ASCII art with flowing gradient text
 *
 * Renders the IMMORTERM block-letter logo with:
 * - Per-character foreground gradient (matching website's ascii-gradient CSS)
 * - Animated gradient flow (colors slide through text, 4s cycle @ 60fps)
 * - 8-stop cyclical gradient per theme from asciiGradient definitions
 *
 * The gradient approach matches apps/web/app/globals.css (.ascii-gradient):
 *   background: linear-gradient(90deg, ...stops);
 *   background-size: 200% 100%;
 *   background-clip: text;
 *   animation: ascii-shimmer 4s linear infinite;
 *
 * In ANSI: foreground-only true color (\x1b[38;2;R;G;Bm) per character.
 * No background colors on the logo — just the terminal's default bg.
 *
 * Performance: Each theme has a 256-entry pre-computed gradient LUT of
 * ANSI fg color strings. Animated rendering is just array lookups — no
 * per-character math at render time.
 *
 * Static banners are pre-computed into BANNER_CACHE at import time.
 * For animated rendering, use renderAnimatedBanner(theme, tick).
 */

import pc from "picocolors";
import {
	THEME_DEFS,
	THEME_NAMES,
	THEME_LABELS,
	THEME_DESCRIPTIONS,
	FREE_THEME_NAMES,
} from "@immorterm/menu-data";

// Re-export shared theme constants so existing consumers keep working
export { THEME_NAMES, THEME_LABELS, THEME_DESCRIPTIONS, FREE_THEME_NAMES as FREE_THEMES };

// ── ASCII Art ──────────────────────────────────────────────────────

const LOGO_LINES = [
	"██╗███╗   ███╗███╗   ███╗ ██████╗ ██████╗ ████████╗███████╗██████╗ ███╗   ███╗",
	"██║████╗ ████║████╗ ████║██╔═══██╗██╔══██╗╚══██╔══╝██╔════╝██╔══██╗████╗ ████║",
	"██║██╔████╔██║██╔████╔██║██║   ██║██████╔╝   ██║   █████╗  ██████╔╝██╔████╔██║",
	"██║██║╚██╔╝██║██║╚██╔╝██║██║   ██║██╔══██╗   ██║   ██╔══╝  ██╔══██╗██║╚██╔╝██║",
	"██║██║ ╚═╝ ██║██║ ╚═╝ ██║╚██████╔╝██║  ██║   ██║   ███████╗██║  ██║██║ ╚═╝ ██║",
	"╚═╝╚═╝     ╚═╝╚═╝     ╚═╝ ╚═════╝ ╚═╝  ╚═╝   ╚═╝   ╚══════╝╚═╝  ╚═╝╚═╝     ╚═╝",
];

const TAGLINE = "Immortal Terminals \u00B7 AI Memory \u00B7 MCP Gateway";
const LOGO_WIDTH = LOGO_LINES[0]!.length;

// ── CLI Theme Definitions ─────────────────────────────────────────
// Shared theme data (statusBarStops, fgAccent, fg, label, description)
// comes from @immorterm/menu-data. The TUI adds asciiGradient per
// theme — 8 cyclical color stops for the animated banner.

import type { ThemeDef } from "@immorterm/menu-data";

interface CliTheme extends ThemeDef {
	asciiGradient: string[];    // 8 cyclical color stops for text gradient
}

/** TUI-specific: 8-stop cyclical gradients for the animated ASCII banner */
const ASCII_GRADIENTS: Record<string, string[]> = {
	"Purple Haze":      ["#7c3aed", "#a855f7", "#06b6d4", "#22c55e", "#f59e0b", "#ef4444", "#a855f7", "#7c3aed"],
	"Ocean Depths":     ["#001f3f", "#0066aa", "#00a0e0", "#87ceeb", "#00a0e0", "#0066aa", "#001f3f", "#0066aa"],
	"Aurora Borealis":  ["#051832", "#1c8b62", "#2ecc71", "#7dffc3", "#2ecc71", "#1c8b62", "#051832", "#1c8b62"],
	"Solar Flare":      ["#8B0000", "#ff4500", "#ff8c00", "#ffd700", "#ff8c00", "#ff4500", "#8B0000", "#ff4500"],
	"Cyberpunk":        ["#ff006e", "#ff00ff", "#00ffff", "#ff006e", "#ff00ff", "#00ffff", "#ff006e", "#ff00ff"],
	"Neon Tokyo":       ["#ff2975", "#ff00ff", "#00ffe5", "#ffe54c", "#00ffe5", "#ff00ff", "#ff2975", "#ff00ff"],
	"Dracula":          ["#bd93f9", "#ff79c6", "#8be9fd", "#50fa7b", "#f1fa8c", "#ff79c6", "#bd93f9", "#ff79c6"],
	"Matrix":           ["#003300", "#00ff41", "#33ff77", "#00ff41", "#00cc33", "#33ff77", "#00ff41", "#003300"],
	"Synthwave":        ["#ff2e97", "#ff6ec7", "#00f3ff", "#ff2e97", "#e040fb", "#00f3ff", "#ff2e97", "#ff6ec7"],
	"Rainbow":          ["#ff0000", "#ff8c00", "#ffd700", "#00ff41", "#00bfff", "#8b00ff", "#ff0000", "#ff8c00"],
	"Sunset Glow":      ["#ff2200", "#ff6347", "#ff8c00", "#ffd700", "#ff8c00", "#ff6347", "#ff2200", "#ff6347"],
	"Glacier":          ["#2266bb", "#4a8ab5", "#7ec8e3", "#b4f0ff", "#7ec8e3", "#4a8ab5", "#2266bb", "#4a8ab5"],
	"Rose Gold":        ["#b95b83", "#d86a99", "#ff69b4", "#ffb6c1", "#ff69b4", "#d86a99", "#b95b83", "#d86a99"],
	"Monochrome Dark":  ["#555555", "#888888", "#bbbbbb", "#ffffff", "#bbbbbb", "#888888", "#555555", "#888888"],
	"Monochrome Light": ["#000000", "#333333", "#666666", "#999999", "#666666", "#333333", "#000000", "#333333"],
	"Vaporwave":        ["#ff71ce", "#b967ff", "#01cdfe", "#00ffd4", "#ff71ce", "#b967ff", "#01cdfe", "#ff71ce"],
	"Ember":            ["#8B2500", "#d4760a", "#ffb84d", "#ff6840", "#d4760a", "#8B2500", "#d4760a", "#ffb84d"],
	"Electric Lime":    ["#2d6a0e", "#588020", "#84cc16", "#beff5a", "#84cc16", "#588020", "#2d6a0e", "#84cc16"],
	"Tidal":            ["#0a4860", "#208890", "#2dd4bf", "#48d1cc", "#2dd4bf", "#208890", "#0a4860", "#2dd4bf"],
	"Amber":            ["#665200", "#887000", "#bfa200", "#ffd700", "#bfa200", "#887000", "#665200", "#bfa200"],
	"Molten":           ["#8B0000", "#b83020", "#ff4500", "#ff6840", "#ff4500", "#b83020", "#8B0000", "#ff6840"],
	"Delulus Club":     ["#2D1864", "#6B3FD6", "#3BC43A", "#F4C21E", "#3BC43A", "#6B3FD6", "#2D1864", "#6B3FD6"],
	"Hot Pink":         ["#7A003C", "#AD0052", "#E0218A", "#FF4FA3", "#E0218A", "#AD0052", "#7A003C", "#AD0052"],
};

/** Merged theme data: shared ThemeDef + TUI-specific asciiGradient */
const CLI_THEMES: Record<string, CliTheme> = {};
for (const [name, def] of Object.entries(THEME_DEFS)) {
	// Themes without a hand-tuned gradient cycle their status-bar stops —
	// an empty array crashes the LUT build at import (Hot Pink incident).
	const fallback = [...def.statusBarStops, ...def.statusBarStops.slice(1, -1).reverse()];
	CLI_THEMES[name] = { ...def, asciiGradient: ASCII_GRADIENTS[name] ?? fallback };
}

// ── Legacy theme name mapping ────────────────────────────────────

const LEGACY_NAMES: Record<string, string> = {
	default: "Purple Haze",
	matrix: "Matrix",
	sunset: "Sunset Glow",
	ocean: "Ocean Depths",
	minimal: "Monochrome Dark",
};

function normalizeName(name: string): string {
	return LEGACY_NAMES[name] ?? (CLI_THEMES[name] ? name : "Purple Haze");
}

// ── Color Helpers ─────────────────────────────────────────────────

type RGB = [number, number, number];

function parseHex(hex: string): RGB {
	return [
		parseInt(hex.slice(1, 3), 16),
		parseInt(hex.slice(3, 5), 16),
		parseInt(hex.slice(5, 7), 16),
	];
}

/** Interpolate through gradient stops at position t (0..1) */
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

/** Foreground-only ANSI colorizer (for menu accents) */
function hexToFgAnsi(hex: string): (s: string) => string {
	const [r, g, b] = parseHex(hex);
	return (s: string) => `\x1b[38;2;${r};${g};${b}m${s}\x1b[0m`;
}

// ── Pre-computed Gradient LUT ────────────────────────────────────
// 256 pre-formatted ANSI fg color strings per theme. Renders become
// pure array lookups — no math, no parseInt, no string formatting.

interface ParsedTheme {
	gradStops: RGB[];
	fgAccent: RGB;
	accentStr: string; // pre-formatted "\x1b[38;2;R;G;Bm"
}

const LUT_SIZE = 256;

/** Gradient LUT: GRADIENT_LUT[themeName][0..255] = "\x1b[38;2;R;G;Bm" */
const GRADIENT_LUT: Record<string, string[]> = {};

const PARSED: Record<string, ParsedTheme> = {};
for (const [name, theme] of Object.entries(CLI_THEMES)) {
	const gradStops = theme.asciiGradient.map(parseHex) as RGB[];
	const fgAccent = parseHex(theme.fgAccent);
	const [aR, aG, aB] = fgAccent;
	PARSED[name] = { gradStops, fgAccent, accentStr: `\x1b[38;2;${aR};${aG};${aB}m` };

	// Build LUT: 256 evenly-spaced gradient samples
	const lut: string[] = new Array(LUT_SIZE);
	for (let i = 0; i < LUT_SIZE; i++) {
		const [r, g, b] = lerpStops(gradStops, i / (LUT_SIZE - 1));
		lut[i] = `\x1b[38;2;${r};${g};${b}m`;
	}
	GRADIENT_LUT[name] = lut;
}

// ── Pre-computed character position tables ────────────────────────
// For each character position in a logo line, store the fractional
// gradient position (0..0.5 for the visible half). Avoids per-frame
// division.

const LOGO_POS: number[] = new Array(LOGO_WIDTH);
{
	const w = LOGO_WIDTH > 1 ? LOGO_WIDTH - 1 : 1;
	for (let i = 0; i < LOGO_WIDTH; i++) {
		LOGO_POS[i] = (i / w) * 0.5;
	}
}

const TAG_LEN = TAGLINE.length;

// Pre-computed tagline positions (same 0..0.5 window, but for tagline width)
const TAG_POS: number[] = new Array(TAG_LEN);
{
	const tw = TAG_LEN > 1 ? TAG_LEN - 1 : 1;
	for (let i = 0; i < TAG_LEN; i++) {
		TAG_POS[i] = (i / tw) * 0.5;
	}
}

// ── Static Banner Cache ───────────────────────────────────────────

function renderStaticLine(line: string, lut: string[], posTable: number[]): string {
	let s = "";
	for (let i = 0; i < line.length; i++) {
		const idx = Math.round(posTable[i]! * (LUT_SIZE - 1));
		s += lut[idx]! + line[i]!;
	}
	return s + "\x1b[0m";
}

/** Pre-computed banner string arrays for all 21 themes — instant lookup */
export const BANNER_CACHE: Record<string, string[]> = {};
for (const name of Object.keys(CLI_THEMES)) {
	const lut = GRADIENT_LUT[name]!;
	const lines: string[] = [""];
	for (const logoLine of LOGO_LINES) {
		lines.push(`  ${renderStaticLine(logoLine, lut, LOGO_POS)}`);
	}
	lines.push("");
	lines.push(`  ${renderStaticLine(TAGLINE, lut, TAG_POS)}`);
	lines.push(`  ${pc.dim("v0.1.0")}`);
	lines.push("");
	BANNER_CACHE[name] = lines;
}

// ── Animated Banner ───────────────────────────────────────────────
// Gradient slides through text like the website's CSS animation:
//   background-size: 200% 100%
//   animation: ascii-shimmer 4s linear infinite
//
// At 15fps, one full cycle = 120 ticks = 8 seconds.
// 15fps avoids terminal flicker while keeping the shimmer smooth.

const ANIM_CYCLE = 120;

/**
 * Render an animated banner frame.
 * Uses pre-computed gradient LUT — per-char cost is one array lookup.
 * @param themeName — theme to render
 * @param tick — animation frame counter (increment by 1 per ~16ms)
 */
export function renderAnimatedBanner(themeName: string, tick: number): string[] {
	const name = normalizeName(themeName);
	const lut = GRADIENT_LUT[name] ?? GRADIENT_LUT["Purple Haze"]!;
	const p = PARSED[name] ?? PARSED["Purple Haze"]!;

	// Phase: 0→1 over ANIM_CYCLE ticks, then wraps
	const phase = (tick % ANIM_CYCLE) / ANIM_CYCLE;
	const lutMax = LUT_SIZE - 1;

	const lines: string[] = [""];

	for (const logoLine of LOGO_LINES) {
		let s = "  ";
		for (let i = 0; i < LOGO_WIDTH; i++) {
			// LOGO_POS[i] is 0..0.5, phase is 0..1. Sum wraps via fractional part.
			const gt = LOGO_POS[i]! + phase;
			const idx = Math.round((gt - Math.floor(gt)) * lutMax);
			s += lut[idx]! + logoLine[i]!;
		}
		lines.push(s + "\x1b[0m");
	}

	// Tagline with flowing gradient (same effect as logo)
	lines.push("");
	{
		let tagStr = "  ";
		for (let i = 0; i < TAG_LEN; i++) {
			const gt = TAG_POS[i]! + phase;
			const idx = Math.round((gt - Math.floor(gt)) * lutMax);
			tagStr += lut[idx]! + TAGLINE[i]!;
		}
		lines.push(tagStr + "\x1b[0m");
	}
	lines.push(`  ${pc.dim("v0.1.0")}`);
	lines.push("");

	return lines;
}

// ── Exports for Theme Picker ──────────────────────────────────────

/** Available theme names in display order */
/** Render a status bar preview swatch — 7 blocks matching the actual bar gradient */
export function renderThemePreview(themeName: string): string {
	const name = normalizeName(themeName);
	const theme = CLI_THEMES[name]!;
	const parts: string[] = [];
	for (const hex of theme.statusBarStops) {
		const [r, g, b] = parseHex(hex);
		parts.push(`\x1b[48;2;${r};${g};${b}m  \x1b[0m`);
	}
	return parts.join("");
}

/**
 * Render the IMMORTERM banner with themed gradient.
 * Returns pre-computed string[] from cache — zero allocation.
 */
export function renderBanner(themeName: string): string[] {
	const name = normalizeName(themeName);
	return BANNER_CACHE[name] ?? BANNER_CACHE["Purple Haze"]!;
}

/** Get the fgAccent colorizer for themed menu highlights */
export function getMenuAccent(themeName: string): (s: string) => string {
	const name = normalizeName(themeName);
	const theme = CLI_THEMES[name]!;
	return hexToFgAnsi(theme.fgAccent);
}

// ── API ────────────────────────────────────────────────────────────

/** Resolve the user's configured theme name (normalized from legacy) */
export function resolveTheme(): string {
	try {
		const { readGlobalConfig } = require("@immorterm/config");
		const config = readGlobalConfig();
		return normalizeName(config.theme ?? "Purple Haze");
	} catch {
		return "Purple Haze";
	}
}
