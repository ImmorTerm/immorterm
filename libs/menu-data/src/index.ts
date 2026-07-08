/**
 * @immorterm/menu-data — Single source of truth for menu items, service
 * definitions, license items, and theme definitions shared between the
 * TUI (`apps/immorterm`) and the GPU terminal webview (`gpu-terminal.html`).
 */

// ── Types ────────────────────────────────────────────────────────

export interface MenuItem {
	id: string;
	label: string;
	desc: string;
}

export interface ServiceDef {
	id: string;
	name: string;
	configKey: "memory" | "mcpGateway";
	desc: string;
	proOnly: boolean;
	canStartStop: boolean;
}

export interface DetailItem {
	id: string;
	label: string;
	desc: string;
}

export interface ThemeDef {
	/** 7 bg gradient stops (dark -> light) for status bar */
	statusBarStops: string[];
	/** Accent color for highlights (hex) */
	fgAccent: string;
	/** Primary foreground color for text on this theme's bg (hex) */
	fg: string;
	/** Emoji-prefixed display label */
	label: string;
	/** One-line description */
	description: string;
}

// ── Menu Items (excludes TUI-specific "quit") ───────────────────

export const MENU_ITEMS: MenuItem[] = [
	{ id: "wizard",   label: "Setup Wizard",  desc: "Configure binary, memory, and services" },
	{ id: "services", label: "Services",      desc: "Manage and control ImmorTerm services" },
	{ id: "insights", label: "Insights",      desc: "View proactive intelligence stats" },
	{ id: "doctor",   label: "Diagnostics",   desc: "Run health checks on all components" },
	{ id: "logs",     label: "Log Explorer",  desc: "Browse terminal session logs" },
	{ id: "pro",      label: "Pro",            desc: "Upgrade or manage your Pro subscription" },
	{ id: "theme",    label: "Theme",         desc: "Change the banner and status bar theme" },
];

// ── Service Definitions ─────────────────────────────────────────

export const SERVICE_DEFS: ServiceDef[] = [
	{
		id: "memory",
		name: "ImmorTerm Memory",
		configKey: "memory",
		desc: "Persistent AI memory — remembers decisions, context, and lessons across sessions",
		proOnly: false,
		canStartStop: true,
	},
	{
		id: "gateway",
		name: "MCP Gateway",
		configKey: "mcpGateway",
		desc: "Shared MCP server proxy — reduces memory ~90% by deduplicating tool processes",
		proOnly: false,
		canStartStop: true,
	},
];

// ── Service Detail Items ────────────────────────────────────────

export function getDetailItems(
	def: ServiceDef,
	enabled: boolean,
): DetailItem[] {
	return [
		{ id: "toggle", label: enabled ? "Disable" : "Enable",
		  desc: `Currently: ${enabled ? "enabled" : "disabled"}` },
		{ id: "start", label: "Start", desc: `Start the ${def.name.toLowerCase()}` },
		{ id: "stop", label: "Stop", desc: `Stop the ${def.name.toLowerCase()}` },
		{ id: "restart", label: "Restart", desc: `Restart the ${def.name.toLowerCase()}` },
	];
}

// ── Pro Sub-menu (excludes TUI-specific "back") ─────────────────

export const PRO_ITEMS_ACTIVE: MenuItem[] = [
	{ id: "status",     label: "View Status",          desc: "Show current license info" },
	{ id: "deactivate", label: "Deactivate (Transfer)", desc: "Move license to another machine" },
	{ id: "pro-page",   label: "Visit immorterm.com/pro", desc: "Manage your subscription online" },
];

export const PRO_ITEMS_FREE: MenuItem[] = [
	{ id: "upgrade",    label: "Upgrade to Pro",  desc: "Unlock unlimited memory, knowledge packs, and more" },
	{ id: "activate",   label: "Activate Key",    desc: "Enter a license key" },
	{ id: "status",     label: "View Status",     desc: "Show current license info" },
];

// Backward compat aliases
export const LICENSE_ITEMS_PRO = PRO_ITEMS_ACTIVE;
export const LICENSE_ITEMS_FREE = PRO_ITEMS_FREE;

// ── Theme Definitions ───────────────────────────────────────────
// 21 themes. statusBarStops, fgAccent, label, and description come
// from banner.ts CLI_THEMES. The `fg` field comes from the webview's
// per-theme foreground color (defaults to #FFFFFF for most dark themes).

export const THEME_DEFS: Record<string, ThemeDef> = {
	"Purple Haze": {
		statusBarStops: ["#2D004D", "#3D1A6D", "#4D2A7D", "#5B2C8A", "#6B3FA0", "#7B52B8", "#8B008B"],
		fgAccent: "#E0B0FF", fg: "#FFFFFF",
		label: "\u{1F7E3} Purple Haze", description: "Purple \u2192 Violet gradient",
	},
	"Ocean Depths": {
		statusBarStops: ["#001F3F", "#003366", "#00447A", "#004C8C", "#0066B3", "#0080D9", "#00A0E0"],
		fgAccent: "#87CEEB", fg: "#FFFFFF",
		label: "\u{1F535} Ocean Depths", description: "Deep blue \u2192 Teal cool",
	},
	"Aurora Borealis": {
		statusBarStops: ["#020B1A", "#051832", "#082E46", "#0C4550", "#105E54", "#1C8B62", "#2ECC71"],
		fgAccent: "#7DFFC3", fg: "#E0FFF4",
		label: "\u{1F30C} Aurora Borealis", description: "Dark \u2192 Green shimmer",
	},
	"Solar Flare": {
		statusBarStops: ["#1A0800", "#331400", "#4D2200", "#6B3500", "#884C00", "#B86B00", "#FF8C00"],
		fgAccent: "#FFD700", fg: "#FFFFFF",
		label: "\u2600\uFE0F Solar Flare", description: "Dark amber \u2192 Blazing gold",
	},
	"Cyberpunk": {
		statusBarStops: ["#0D0221", "#1A0A3E", "#240E4C", "#2D1259", "#541388", "#7B1FA2", "#FF006E"],
		fgAccent: "#FF00FF", fg: "#00FFFF",
		label: "\u{1F49C} Cyberpunk", description: "Neon purple \u2192 Magenta glow",
	},
	"Neon Tokyo": {
		statusBarStops: ["#080011", "#140828", "#281044", "#421860", "#661E74", "#AA1E6E", "#FF2975"],
		fgAccent: "#FFE54C", fg: "#00FFE5",
		label: "\u{1F3D9}\uFE0F Neon Tokyo", description: "Midnight \u2192 Neon pink city",
	},
	"Dracula": {
		statusBarStops: ["#21222C", "#282A36", "#2E303E", "#343746", "#44475A", "#6272A4", "#BD93F9"],
		fgAccent: "#FF79C6", fg: "#F8F8F2",
		label: "\u{1F9DB} Dracula", description: "Classic Dracula color scheme",
	},
	"Matrix": {
		statusBarStops: ["#000000", "#001500", "#002A00", "#004200", "#005E00", "#008B00", "#00FF41"],
		fgAccent: "#33FF77", fg: "#00FF41",
		label: "\u{1F48A} Matrix", description: "Black \u2192 Green digital rain",
	},
	"Synthwave": {
		statusBarStops: ["#1A1A2E", "#262640", "#2C2C49", "#323252", "#4A3F6B", "#614385", "#FF2E97"],
		fgAccent: "#00F3FF", fg: "#FFFFFF",
		label: "\u{1F306} Synthwave", description: "Retro purple \u2192 Electric cyan",
	},
	"Rainbow": {
		statusBarStops: ["#8B0000", "#8B4500", "#6B6B00", "#006400", "#00008B", "#4B0082", "#800080"],
		fgAccent: "#FFD700", fg: "#FFFFFF",
		label: "\u{1F308} Rainbow", description: "Full spectrum gradient",
	},
	"Sunset Glow": {
		statusBarStops: ["#4A1C1C", "#6B2D2D", "#7E3636", "#8C3E3E", "#AD4F4F", "#CE6060", "#E07020"],
		fgAccent: "#FFD700", fg: "#FFFFFF",
		label: "\u{1F7E0} Sunset Glow", description: "Warm red \u2192 Gold horizon",
	},
	"Glacier": {
		statusBarStops: ["#06111C", "#0E2240", "#163560", "#224D7E", "#35689A", "#4A8AB5", "#7EC8E3"],
		fgAccent: "#B4F0FF", fg: "#F0F8FF",
		label: "\u{1F3D4}\uFE0F Glacier", description: "Frozen blue \u2192 Ice white",
	},
	"Rose Gold": {
		statusBarStops: ["#3D1F2B", "#5C2E41", "#6C364C", "#7B3D57", "#9A4C6D", "#B95B83", "#D86A99"],
		fgAccent: "#FFB6C1", fg: "#FFFFFF",
		label: "\u{1FA77} Rose Gold", description: "Deep rose \u2192 Pink bloom",
	},
	"Monochrome Dark": {
		statusBarStops: ["#000000", "#1A1A1A", "#2D2D2D", "#404040", "#555555", "#6A6A6A", "#808080"],
		fgAccent: "#CCCCCC", fg: "#FFFFFF",
		label: "\u2B1B Monochrome Dark", description: "Clean black \u2192 Gray minimal",
	},
	"Monochrome Light": {
		statusBarStops: ["#FFFFFF", "#F0F0F0", "#E0E0E0", "#D0D0D0", "#C0C0C0", "#B0B0B0", "#A0A0A0"],
		fgAccent: "#333333", fg: "#000000",
		label: "\u2B1C Monochrome Light", description: "White \u2192 Silver light mode",
	},
	"Vaporwave": {
		statusBarStops: ["#0A1520", "#1A1E30", "#2E2440", "#442A52", "#603066", "#983878", "#FF71CE"],
		fgAccent: "#01CDFE", fg: "#00FFD4",
		label: "\u{1F4FC} Vaporwave", description: "Retro purple \u2192 Pink aesthetic",
	},
	"Ember": {
		statusBarStops: ["#1A0A00", "#2E1400", "#441E08", "#5C2A10", "#763818", "#994520", "#D4760A"],
		fgAccent: "#FFB84D", fg: "#FFF5E6",
		label: "\u{1F525} Ember", description: "Smoldering dark \u2192 Warm amber",
	},
	"Electric Lime": {
		statusBarStops: ["#050A00", "#101C05", "#1E3008", "#2E4610", "#406018", "#588020", "#84CC16"],
		fgAccent: "#BEFF5A", fg: "#F0FFF0",
		label: "\u26A1 Electric Lime", description: "Dark forest \u2192 Bright lime",
	},
	"Tidal": {
		statusBarStops: ["#020A18", "#061530", "#0A2848", "#104060", "#186078", "#208890", "#2DD4BF"],
		fgAccent: "#48D1CC", fg: "#E0FFFF",
		label: "\u{1F30A} Tidal", description: "Deep ocean \u2192 Teal wave",
	},
	"Amber": {
		statusBarStops: ["#0E0A00", "#1E1800", "#302800", "#483C00", "#665200", "#887000", "#BFA200"],
		fgAccent: "#FFD700", fg: "#FFFDD0",
		label: "\u{1F7E1} Amber", description: "Dark gold \u2192 Bright amber",
	},
	"Molten": {
		statusBarStops: ["#100004", "#200810", "#381018", "#501820", "#702020", "#983028", "#B83020"],
		fgAccent: "#FF6840", fg: "#FFD8C8",
		label: "\u{1F30B} Molten", description: "Black lava \u2192 Fiery red",
	},
	"Delulus Club": {
		statusBarStops: ["#2D1864", "#4A26A8", "#6B3FD6", "#3BC43A", "#3BC43A", "#3BC43A", "#3BC43A"],
		fgAccent: "#F4C21E", fg: "#FBF7F0",
		label: "\u{1F331} Delulus Club", description: "Indigo \u2192 Purple \u2192 dominant Green",
	},
	"Hot Pink": {
		statusBarStops: ["#14000A", "#260012", "#3D001E", "#57002B", "#7A003C", "#AD0052", "#E0218A"],
		fgAccent: "#FF4FA3", fg: "#FFD9EC",
		label: "\u{1F497} Hot Pink", description: "Black \u2192 Hot magenta glow",
	},
};

// ── Derived Constants ───────────────────────────────────────────

/** All theme names in definition order */
export const THEME_NAMES = Object.keys(THEME_DEFS);

/** The 5 themes available on the Free tier */
export const FREE_THEME_NAMES = new Set([
	"Purple Haze",
	"Ocean Depths",
	"Matrix",
	"Sunset Glow",
	"Monochrome Dark",
]);

/** Emoji-prefixed display labels keyed by theme name */
export const THEME_LABELS: Record<string, string> = {};
for (const [name, theme] of Object.entries(THEME_DEFS)) {
	THEME_LABELS[name] = theme.label;
}

/** One-line theme descriptions keyed by theme name */
export const THEME_DESCRIPTIONS: Record<string, string> = {};
for (const [name, theme] of Object.entries(THEME_DEFS)) {
	THEME_DESCRIPTIONS[name] = theme.description;
}

// ── Speak Mode (AI Character) ───────────────────────────────────
// Character metadata registry. The long-form system-prompt body for
// each character lives in `apps/immorterm-ai/characters/<promptFile>`
// as markdown with YAML frontmatter — loaded at runtime by the user-
// prompt hook and injected as an <speak_mode> XML block. Keeping
// metadata here (typed, shared) and prompt bodies in markdown mirrors
// the TUI/webview split used for themes.

export interface CharacterDef {
	/** Emoji-prefixed display label (e.g. "🪨 Caveman") */
	label: string;
	/** One-line description for menus */
	description: string;
	/** Standalone emoji, used for the sidebar badge when this character
	 * is active for a session. Empty string for the default character. */
	emoji: string;
	/** Filename under `apps/immorterm-ai/characters/` that holds the
	 * system-prompt body. Frontmatter is stripped at injection time. */
	promptFile: string;
}

export const CHARACTER_DEFS: Record<string, CharacterDef> = {
	default: {
		label: "🤖 Default",
		description: "Vendor's native voice — no character override",
		emoji: "",
		promptFile: "default.md",
	},
	caveman: {
		label: "🪨 Caveman",
		description: "UGG SPEAK SIMPLE — caveman-style responses",
		emoji: "🪨",
		promptFile: "caveman.md",
	},
	ceo: {
		label: "📊 CEO",
		description: "Executive bottom-line — status, what's left, decision needed",
		emoji: "📊",
		promptFile: "ceo.md",
	},
};

/** All character IDs in definition order (default first) */
export const CHARACTER_IDS = Object.keys(CHARACTER_DEFS);

/** The default character ID when no override is set */
export const DEFAULT_CHARACTER_ID = "default";

// ── Digest LLM ──────────────────────────────────────────────────
// Models per provider for the digest LLM picker (Phase A T11).
// Static lists; ollama / llm-cli are queried at runtime so they
// don't appear here. The first entry in each list is the default.

export interface DigestModelOption {
	/** Model identifier passed to the provider's API/CLI */
	id: string;
	/** Short human label shown in the picker */
	label: string;
	/** One-line description shown as `description` in QuickPick */
	description: string;
}

// Source of truth: live vendor docs, verified 2026-05-07. Refresh quarterly.
//   - Anthropic: https://platform.claude.com/docs/en/about-claude/models
//   - OpenAI:    https://developers.openai.com/api/docs/models/all
//   - Gemini:    https://ai.google.dev/gemini-api/docs/models
//   - Copilot:   https://docs.github.com/en/copilot/reference/ai-models/supported-models
//   - LiteLLM (cross-vendor cross-check): https://github.com/BerriAI/litellm/blob/main/model_prices_and_context_window.json
//
// Curation rules:
//   1. Lead with each vendor's *current* recommendation (per vendor's own docs).
//   2. Include the vendor's "latest" alias only if that alias is documented
//      as currently valid (don't ship aliases like gemini-pro-latest that
//      aren't in vendor docs even if they look plausible).
//   3. NEVER include deprecated/retired IDs \u2014 they make the picker into a
//      foot-gun for users who don't track vendor announcements.
export const DIGEST_MODELS: Record<string, DigestModelOption[]> = {
	anthropic: [
		// Claude Code CLI accepts `sonnet` and `opus` as latest-aliases per
		// code.claude.com/docs/en/cli-reference (e.g. `claude --model sonnet`).
		// `haiku` is included by symmetry \u2014 the digester shim already uses
		// "sonnet" as its default and works.
		{ id: "sonnet", label: "Claude Sonnet (latest)", description: "Auto-updates, balanced (recommended)" },
		{ id: "haiku", label: "Claude Haiku (latest)", description: "Fastest + cheapest" },
		{ id: "opus", label: "Claude Opus (latest)", description: "Highest quality, pricier" },
		// Pinned current generation (2026-05): Sonnet 4.6, Opus 4.7, Haiku 4.5.
		// Note: there is NO claude-sonnet-4-7 \u2014 4.6 is the current Sonnet.
		{ id: "claude-opus-4-7", label: "Claude Opus 4.7", description: "Most capable (Apr 2026)" },
		{ id: "claude-sonnet-4-6", label: "Claude Sonnet 4.6", description: "Best speed+intel balance" },
		{ id: "claude-haiku-4-5", label: "Claude Haiku 4.5", description: "Pinned cheap+fast (Oct 2025)" },
	],
	openai: [
		// gpt-5.5 is OpenAI's current default per developers.openai.com (Apr
		// 2026 release). gpt-5-chat-latest, o3-mini, o4-mini, chatgpt-4o-latest
		// are all DEPRECATED \u2014 do not re-add.
		{ id: "gpt-5.5", label: "GPT-5.5", description: "Current default (Apr 2026)" },
		{ id: "gpt-5.5-pro", label: "GPT-5.5 Pro", description: "Highest quality, pricier" },
		{ id: "gpt-5-mini", label: "GPT-5 mini", description: "Cost-sensitive, low-latency" },
		{ id: "gpt-5-nano", label: "GPT-5 nano", description: "Cheapest, fastest" },
		{ id: "gpt-5.4", label: "GPT-5.4", description: "Previous generation, still supported" },
	],
	gemini: [
		// Gemini 3.x family is in preview (May 2026) but already used in
		// production in production via the AI Studio API. The
		// 2.5 family is the GA fallback for Vertex AI users (Vertex only
		// hosts GA models). Notes:
		//   - gemini-3-pro-preview was SHUT DOWN 2026-03-09; use 3.1-pro instead.
		//   - gemini-pro-latest is NOT a documented alias.
		//   - Preview IDs aren't available on Vertex AI \u2014 stick to 2.5 there.
		{ id: "gemini-3.1-pro-preview", label: "Gemini 3.1 Pro (preview)", description: "Most capable preview" },
		{ id: "gemini-3-flash-preview", label: "Gemini 3 Flash (preview)", description: "Recommended balanced preview" },
		{ id: "gemini-3.1-flash-lite-preview", label: "Gemini 3.1 Flash Lite (preview)", description: "Cheapest preview" },
		{ id: "gemini-2.5-flash", label: "Gemini 2.5 Flash", description: "GA workhorse (Vertex-compatible)" },
		{ id: "gemini-2.5-pro", label: "Gemini 2.5 Pro", description: "GA higher quality (Vertex-compatible)" },
		{ id: "gemini-flash-latest", label: "Gemini Flash (latest)", description: "Auto-tracks current Flash GA" },
	],
	// GitHub Copilot CLI default per github/copilot-cli README is Claude
	// Sonnet 4.5. Other CLI-supported: GPT-5 mini, GPT-5.3-Codex, GPT-5.4,
	// Claude Haiku 4.5, Claude Sonnet 4.6. Format unclear from public docs
	// (Copilot's display uses "Claude Sonnet 4.5" with dot), so we ship dot
	// notation matching the vendor's display; users with the CLI can run
	// `/model` interactively to see the canonical menu.
	copilot: [
		{ id: "claude-sonnet-4.5", label: "Claude Sonnet 4.5 (via Copilot)", description: "Default per github/copilot-cli" },
		{ id: "claude-sonnet-4.6", label: "Claude Sonnet 4.6 (via Copilot)", description: "Latest Sonnet via Copilot" },
		{ id: "claude-haiku-4.5", label: "Claude Haiku 4.5 (via Copilot)", description: "Cheapest Anthropic via Copilot" },
		{ id: "gpt-5.4", label: "GPT-5.4 (via Copilot)", description: "OpenAI top tier via Copilot" },
		{ id: "gpt-5-mini", label: "GPT-5 mini (via Copilot)", description: "Faster, cheaper" },
	],
	// opencode uses `provider/model-id` format and routes via the
	// models.dev catalog (https://models.dev/api.json). opencode.ai/docs's
	// "recommended" list explicitly admits it's "not necessarily up to
	// date" \u2014 so we refresh from models.dev directly. Verified 2026-05-07
	// against the live catalog. Users can run `opencode models` at runtime
	// to see their full provider-configured set.
	opencode: [
		{ id: "anthropic/claude-opus-4-7", label: "Anthropic Opus 4.7", description: "Most capable Anthropic (Apr 2026)" },
		{ id: "anthropic/claude-sonnet-4-6", label: "Anthropic Sonnet 4.6", description: "Balanced (recommended)" },
		{ id: "anthropic/claude-haiku-4-5", label: "Anthropic Haiku 4.5", description: "Fast + cheap" },
		{ id: "openai/gpt-5.4", label: "OpenAI GPT-5.4", description: "Latest OpenAI in models.dev" },
		{ id: "openai/gpt-5.4-pro", label: "OpenAI GPT-5.4 Pro", description: "Highest quality OpenAI" },
		{ id: "google/gemini-3-pro-preview", label: "Google Gemini 3 Pro (preview)", description: "Google top tier" },
		{ id: "google/gemini-3-flash-preview", label: "Google Gemini 3 Flash (preview)", description: "Google fast preview" },
		{ id: "opencode/gpt-5.1-codex", label: "OpenCode Zen \u2014 GPT-5.1 Codex", description: "OpenCode Zen sub" },
	],
};
