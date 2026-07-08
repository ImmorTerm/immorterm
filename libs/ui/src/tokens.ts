/**
 * @immorterm/ui tokens — derived from the product's own theme system.
 *
 * Colors come from libs/menu-data THEME_DEFS (the canonical 7-color
 * statusBarStops palettes). Default brand theme: "Purple Haze".
 * CSS custom properties live in ./styles.css; use `themeCssVars()` to
 * switch themes at runtime via an inline style on any container.
 */

import { THEME_DEFS, type ThemeDef } from "@immorterm/menu-data";

export { THEME_DEFS };
export type { ThemeDef };

export const DEFAULT_THEME = "Purple Haze";

const defaultTheme = THEME_DEFS[DEFAULT_THEME];
if (!defaultTheme) throw new Error(`Missing theme "${DEFAULT_THEME}" in THEME_DEFS`);

/** The default brand palette (Purple Haze). */
export const brand: ThemeDef = defaultTheme;

/**
 * CSS variables for a theme, suitable for `style={themeCssVars("Matrix")}`.
 * Falls back to the default theme for unknown names.
 */
export function themeCssVars(name: string = DEFAULT_THEME): Record<string, string> {
	const t = THEME_DEFS[name] ?? brand;
	const vars: Record<string, string> = {};
	t.statusBarStops.forEach((stop, i) => {
		vars[`--theme-${i + 1}`] = stop;
	});
	vars["--theme-accent"] = t.fgAccent;
	vars["--theme-fg"] = t.fg;
	return vars;
}

// ── Mort brand constants (the brand design tokens) ──────────
// Brand surfaces only (landing, /memory, docs, stickers). Product
// surfaces keep the user's THEME_DEFS theme — these never replace it.
export const mort = {
	tank: "#0B0D14",
	tankGlass: "#12151F",
	tankLine: "#232838",
	foam: "#F4F2F5",
	foamSoft: "#9AA0B0",
	ink: "#252A3D",
	body: "#F7CFD6",
	frond: "#EF8189",
	coral: "#FF5C73",
	coralDeep: "#E5455D",
	// v2 water column (design-lock v2 §2) — additive, nothing renamed
	coralWash: "#FFE1E7",
	aqua: "#3EE6D2",
	lagoon: "#0FA3A3",
	deepwater: "#0A2E3D",
	vitals: "#4ADE80",
	warn: "#F5A54C",
	danger: "#E0546C",
} as const;

// ── Spacing (px) ─────────────────────────────────────────────────
export const spacing = {
	xs: 4,
	sm: 8,
	md: 16,
	lg: 24,
	xl: 40,
	"2xl": 64,
	"3xl": 96,
} as const;

// ── Radii (px) ───────────────────────────────────────────────────
export const radii = {
	sm: 6,
	md: 10,
	lg: 16,
	xl: 24,
	full: 9999,
} as const;

// ── Type scale (rem) ─────────────────────────────────────────────
export const typeScale = {
	xs: "0.75rem",
	sm: "0.875rem",
	base: "1rem",
	lg: "1.125rem",
	xl: "1.375rem",
	"2xl": "1.75rem",
	"3xl": "2.25rem",
	"4xl": "3rem",
	"5xl": "3.75rem",
} as const;

// ── Motion (mirrors docs/design-system motion tokens) ────────────
export const motion = {
	instant: "100ms",
	fast: "180ms",
	base: "260ms",
	slow: "420ms",
} as const;

export const easing = {
	spring: "cubic-bezier(0.32, 0.72, 0, 1)",
	out: "cubic-bezier(0.25, 0.46, 0.45, 0.94)",
} as const;

export const fonts = {
	sans: '"Inter", system-ui, sans-serif',
	mono: '"JetBrains Mono", "Fira Code", monospace',
} as const;
