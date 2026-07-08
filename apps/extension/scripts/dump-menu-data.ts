// One-shot codegen: exports @immorterm/menu-data constants as JSON for the
// hub to serve in standalone (Tauri/browser) mode.
//
// Run: bun apps/extension/scripts/dump-menu-data.ts > <static-dir>/menu-data.json
import {
  MENU_ITEMS,
  SERVICE_DEFS,
  LICENSE_ITEMS_PRO,
  LICENSE_ITEMS_FREE,
  THEME_DEFS,
  THEME_NAMES,
  FREE_THEME_NAMES,
  CHARACTER_DEFS,
  DEFAULT_CHARACTER_ID,
} from '../../../libs/menu-data/src/index';

const themes = THEME_NAMES.map((name) => {
  const def = THEME_DEFS[name]!;
  return {
    name,
    label: def.label,
    desc: def.description,
    bg: def.statusBarStops[0],
    accent: def.fgAccent,
    fg: def.fg,
    stops: def.statusBarStops,
  };
});

console.log(
  JSON.stringify(
    {
      menuItems: MENU_ITEMS,
      serviceDefs: SERVICE_DEFS,
      licenseItemsPro: LICENSE_ITEMS_PRO,
      licenseItemsFree: LICENSE_ITEMS_FREE,
      themes,
      freeThemes: [...FREE_THEME_NAMES],
      characterDefs: CHARACTER_DEFS,
      defaultCharacterId: DEFAULT_CHARACTER_ID,
    },
    null,
    2,
  ),
);
