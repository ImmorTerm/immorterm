#!/usr/bin/env node
/**
 * Regenerate apps/extension/resources/menu-data.json from the
 * @immorterm/menu-data lib. The Tauri hub's /api/v1/config endpoint
 * reads this file at runtime — when it's missing, clients see 0
 * themes and a minimal hardcoded menu (silent regression), so we
 * make this a required build step.
 *
 * Source of truth: libs/menu-data/dist/index.js in the main immorterm
 * repo. Loaded via a dynamic require so this script works from either
 * the Tauri repo or the main repo — whichever path it's run from.
 */

import { createRequire } from "node:module";
import { writeFileSync, existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const TAURI_ROOT = resolve(HERE, "..");
// Monorepo layout first (repo/libs/menu-data — what CI checks out), then the
// sibling-worktree layout (immorterm-tauri next to immorterm) used locally.
const LIB_CANDIDATES = [
  resolve(HERE, "../libs/menu-data/dist/index.js"),
  resolve(HERE, "../../immorterm/libs/menu-data/dist/index.js"),
];
const MAIN_REPO_LIB = LIB_CANDIDATES.find(existsSync);

if (!MAIN_REPO_LIB) {
  console.error(`[menu-data] lib not found at any of:\n  ${LIB_CANDIDATES.join("\n  ")}`);
  console.error(`[menu-data] run: cd <main-repo>/libs/menu-data && bun run build`);
  process.exit(1);
}

const req = createRequire(import.meta.url);
const m = req(MAIN_REPO_LIB);

// Normalize THEME_DEFS record into an array of {name, ...theme} so
// hub clients can iterate without inventing key/value order.
const themes = Object.entries(m.THEME_DEFS || {}).map(([name, t]) => ({
  name,
  ...t,
}));

// CHARACTER_DEFS may or may not be in dist depending on lib build age.
// When present, emit as array. When absent, leave empty — callers that
// need speak-mode characters will fall back to the bundled personas.
const characters = m.CHARACTER_DEFS
  ? Object.entries(m.CHARACTER_DEFS).map(([id, c]) => ({ id, ...c }))
  : [];

const out = {
  menuItems: m.MENU_ITEMS || [],
  serviceDefs: m.SERVICE_DEFS || [],
  licenseItemsPro: m.LICENSE_ITEMS_PRO || [],
  licenseItemsFree: m.LICENSE_ITEMS_FREE || [],
  proItemsActive: m.PRO_ITEMS_ACTIVE || [],
  proItemsFree: m.PRO_ITEMS_FREE || [],
  themes,
  themeNames: m.THEME_NAMES || [],
  freeThemeNames: m.FREE_THEME_NAMES || [],
  characters,
  characterIds: characters.map((c) => c.id),
  defaultCharacterId: m.DEFAULT_CHARACTER_ID || "default",
};

const dest = join(TAURI_ROOT, "apps", "extension", "resources", "menu-data.json");
writeFileSync(dest, JSON.stringify(out, null, 2));

console.log(
  `[menu-data] wrote ${dest} — themes=${themes.length} menuItems=${out.menuItems.length} services=${out.serviceDefs.length} characters=${characters.length}`,
);
