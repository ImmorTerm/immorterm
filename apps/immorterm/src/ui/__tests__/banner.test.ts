import { describe, it, expect, vi, beforeEach } from "vitest";
import {
	THEME_NAMES,
	THEME_LABELS,
	THEME_DESCRIPTIONS,
	FREE_THEMES,
	BANNER_CACHE,
	renderThemePreview,
	renderBanner,
	renderAnimatedBanner,
	getMenuAccent,
	resolveTheme,
} from "../banner.js";

// ── Mocks ────────────────────────────────────────────────────────

vi.mock("@immorterm/config", () => ({
	readGlobalConfig: vi.fn(() => ({ theme: "Purple Haze" })),
}));

beforeEach(() => {
	vi.clearAllMocks();
});

// ── THEME_NAMES ──────────────────────────────────────────────────

describe("THEME_NAMES", () => {
	it("contains all 23 themes", () => {
		expect(THEME_NAMES).toHaveLength(23);
	});

	it("contains the 5 free themes", () => {
		expect(THEME_NAMES).toContain("Purple Haze");
		expect(THEME_NAMES).toContain("Ocean Depths");
		expect(THEME_NAMES).toContain("Matrix");
		expect(THEME_NAMES).toContain("Sunset Glow");
		expect(THEME_NAMES).toContain("Monochrome Dark");
	});

	it("contains pro themes", () => {
		expect(THEME_NAMES).toContain("Aurora Borealis");
		expect(THEME_NAMES).toContain("Cyberpunk");
		expect(THEME_NAMES).toContain("Dracula");
		expect(THEME_NAMES).toContain("Rainbow");
		expect(THEME_NAMES).toContain("Neon Tokyo");
	});
});

// ── THEME_LABELS ─────────────────────────────────────────────────

describe("THEME_LABELS", () => {
	it("has a label for every theme name", () => {
		for (const name of THEME_NAMES) {
			expect(THEME_LABELS[name]).toBeDefined();
			expect(THEME_LABELS[name]!.length).toBeGreaterThan(0);
		}
	});

	it("labels contain the theme name", () => {
		for (const name of THEME_NAMES) {
			expect(THEME_LABELS[name]).toContain(name);
		}
	});
});

// ── THEME_DESCRIPTIONS ───────────────────────────────────────────

describe("THEME_DESCRIPTIONS", () => {
	it("has a description for every theme name", () => {
		for (const name of THEME_NAMES) {
			expect(THEME_DESCRIPTIONS[name]).toBeDefined();
			expect(typeof THEME_DESCRIPTIONS[name]).toBe("string");
			expect(THEME_DESCRIPTIONS[name]!.length).toBeGreaterThan(0);
		}
	});
});

// ── FREE_THEMES ──────────────────────────────────────────────────

describe("FREE_THEMES", () => {
	it("has exactly 5 free themes", () => {
		expect(FREE_THEMES.size).toBe(5);
	});

	it("contains the expected free themes", () => {
		expect(FREE_THEMES.has("Purple Haze")).toBe(true);
		expect(FREE_THEMES.has("Ocean Depths")).toBe(true);
		expect(FREE_THEMES.has("Matrix")).toBe(true);
		expect(FREE_THEMES.has("Sunset Glow")).toBe(true);
		expect(FREE_THEMES.has("Monochrome Dark")).toBe(true);
	});

	it("does not include pro themes", () => {
		expect(FREE_THEMES.has("Rainbow")).toBe(false);
		expect(FREE_THEMES.has("Cyberpunk")).toBe(false);
	});
});

// ── renderThemePreview ───────────────────────────────────────────

describe("renderThemePreview", () => {
	it("returns a non-empty string for each theme", () => {
		for (const name of THEME_NAMES) {
			const preview = renderThemePreview(name);
			expect(preview.length).toBeGreaterThan(0);
		}
	});

	it("contains ANSI escape codes for background true color", () => {
		const preview = renderThemePreview("Purple Haze");
		expect(preview).toContain("\x1b[48;2;");
	});

	it("falls back to Purple Haze preview for unknown theme", () => {
		const preview = renderThemePreview("nonexistent");
		expect(preview.length).toBeGreaterThan(0);
	});
});

// ── renderBanner ─────────────────────────────────────────────────

describe("renderBanner", () => {
	it("returns an array of strings", () => {
		const lines = renderBanner("Purple Haze");
		expect(Array.isArray(lines)).toBe(true);
		expect(lines.length).toBeGreaterThan(0);
	});

	it("includes the logo block art", () => {
		const lines = renderBanner("Purple Haze");
		const joined = lines.join("\n");
		// Per-character rendering puts ANSI escapes between each char
		// so check for block char after an ANSI escape
		expect(joined).toContain("m\u2588");
	});

	it("includes the tagline characters (gradient-colored)", () => {
		const lines = renderBanner("Purple Haze");
		const joined = lines.join("\n");
		// Tagline is per-char gradient: ANSI escapes between each letter
		// Check for "mI" (ANSI ending + 'I') followed later by "mT" (for "Terminals")
		expect(joined).toContain("mI");
		expect(joined).toContain("mT");
		expect(joined).toContain("mG"); // "Gateway"
	});

	it("uses true color foreground escapes (no background)", () => {
		const lines = renderBanner("Matrix");
		const joined = lines.join("\n");
		// Gradient is foreground-only (matching website's background-clip: text)
		expect(joined).toContain("\x1b[38;2;");
		expect(joined).not.toContain("\x1b[48;2;");
	});

	it("falls back for unknown theme", () => {
		const lines = renderBanner("nonexistent");
		expect(lines.length).toBeGreaterThan(0);
	});

	it("handles legacy theme name 'default'", () => {
		const lines = renderBanner("default");
		expect(lines.length).toBeGreaterThan(0);
		// Should resolve to Purple Haze
		expect(lines).toEqual(renderBanner("Purple Haze"));
	});

	it("handles legacy theme name 'matrix'", () => {
		const lines = renderBanner("matrix");
		expect(lines).toEqual(renderBanner("Matrix"));
	});
});

// ── BANNER_CACHE ─────────────────────────────────────────────────

describe("BANNER_CACHE", () => {
	it("has a pre-computed banner for every theme", () => {
		for (const name of THEME_NAMES) {
			expect(BANNER_CACHE[name]).toBeDefined();
			expect(BANNER_CACHE[name]!.length).toBeGreaterThan(0);
		}
	});

	it("returns same reference as renderBanner", () => {
		// renderBanner should return from cache, not allocate new arrays
		for (const name of THEME_NAMES) {
			expect(renderBanner(name)).toBe(BANNER_CACHE[name]);
		}
	});
});

// ── getMenuAccent ────────────────────────────────────────────────

describe("getMenuAccent", () => {
	it("returns a function", () => {
		const fn = getMenuAccent("Purple Haze");
		expect(typeof fn).toBe("function");
	});

	it("colorizes text with ANSI escapes", () => {
		const fn = getMenuAccent("Purple Haze");
		const colored = fn("test");
		expect(colored).toContain("test");
		expect(colored).toContain("\x1b[38;2;");
	});
});

// ── renderAnimatedBanner ─────────────────────────────────────────

describe("renderAnimatedBanner", () => {
	it("returns an array of strings", () => {
		const lines = renderAnimatedBanner("Purple Haze", 0);
		expect(Array.isArray(lines)).toBe(true);
		expect(lines.length).toBeGreaterThan(0);
	});

	it("includes the logo block art", () => {
		const joined = renderAnimatedBanner("Purple Haze", 0).join("\n");
		// Per-character rendering: check for block char after ANSI escape
		expect(joined).toContain("m\u2588");
	});

	it("uses foreground-only ANSI escapes (gradient text)", () => {
		const joined = renderAnimatedBanner("Matrix", 0).join("\n");
		// Foreground gradient, no background colors
		expect(joined).toContain("\x1b[38;2;");
		expect(joined).not.toContain("\x1b[48;2;");
	});

	it("produces different output at different ticks (shimmer moves)", () => {
		const frame0 = renderAnimatedBanner("Purple Haze", 0).join("");
		const frame20 = renderAnimatedBanner("Purple Haze", 20).join("");
		expect(frame0).not.toBe(frame20);
	});

	it("falls back for unknown theme", () => {
		const lines = renderAnimatedBanner("nonexistent", 0);
		expect(lines.length).toBeGreaterThan(0);
	});
});

// ── resolveTheme ─────────────────────────────────────────────────

describe("resolveTheme", () => {
	it("returns a valid theme name string", () => {
		const result = resolveTheme();
		expect(typeof result).toBe("string");
		expect(result.length).toBeGreaterThan(0);
	});

	it("returns 'Purple Haze' as the fallback", () => {
		expect(resolveTheme()).toBe("Purple Haze");
	});
});
