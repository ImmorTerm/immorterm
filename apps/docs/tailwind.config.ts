import { createPreset } from "fumadocs-ui/tailwind-plugin";
import type { Config } from "tailwindcss";
// Tank palette single source of truth (the brand design tokens).
// Import the tokens file directly (pure TS) — the @immorterm/ui barrel pulls
// in .tsx components that jiti (tailwind's TS config loader) can't transpile.
import { mort } from "../../libs/ui/src/tokens";

export default {
	content: [
		"./app/**/*.{ts,tsx}",
		"./content/**/*.{md,mdx}",
		"./node_modules/fumadocs-ui/dist/**/*.js",
		// @immorterm/ui components (Nav, Footer, ...) are compiled in-app — scan them
		// so their utility classes are emitted.
		"../../libs/ui/src/**/*.{ts,tsx}",
	],
	presets: [createPreset()],
	theme: {
		extend: {
			// The token classes @immorterm/ui components use (bg-bg, text-text-muted,
			// text-accent, ...) — Tailwind-3 equivalents of apps/web's @theme block.
			colors: {
				brand: { DEFAULT: mort.coral, dark: mort.coralDeep },
				accent: mort.coral,
				bg: {
					DEFAULT: mort.tank,
					card: mort.tankGlass,
					// ponytail: bg-elevated exists only in apps/web's @theme, not tokens.ts;
					// promote it there if a third consumer appears.
					elevated: "#1a1e2b",
				},
				"text-primary": mort.foam,
				"text-muted": mort.foamSoft,
				success: mort.vitals,
				warning: mort.warn,
				danger: mort.danger,
			},
			fontFamily: {
				sans: ['"Inter"', "system-ui", "sans-serif"],
				mono: ['"JetBrains Mono"', '"Fira Code"', "monospace"],
			},
		},
	},
} satisfies Config;
