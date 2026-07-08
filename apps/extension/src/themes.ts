/**
 * ImmorTerm Status Bar Themes
 *
 * Each theme defines a gradient of colors for the status bar:
 * - bg1: Project name background (darkest/leftmost)
 * - bg2: Separator "/"
 * - bg3: Window title
 * - bg4: "Last Active:" label
 * - bg5: Timestamp value (uses %I escape - zero polling!)
 * - bg6: "ImmorTerm" branding (lightest/rightmost)
 * - fg: Default foreground (text) color
 * - fgAccent: Accent foreground for "Last Active:" label
 */

export interface Theme {
  name: string;
  bg1: string;  // Project name (darkest)
  bg2: string;  // Separator "/"
  bg3: string;  // Window title
  bg4: string;  // "Last Active:" label
  bg5: string;  // Timestamp value
  bg6: string;  // "ImmorTerm" branding (was memory, now branding)
  bg7: string;  // Kept for backward compatibility
  fg: string;
  fgAccent: string;
  multiStopGradient?: boolean;  // Use all 7 bg stops instead of just bg1→bg6
}

export const themes: Record<string, Theme> = {
  'Purple Haze': {
    name: 'Purple Haze',
    bg1: '#2D004D',
    bg2: '#3D1A6D',
    bg3: '#4D2A7D',
    bg4: '#5B2C8A',
    bg5: '#6B3FA0',
    bg6: '#7B52B8',
    bg7: '#8B008B',
    fg: '#FFFFFF',
    fgAccent: '#E0B0FF',
  },
  'Ocean Depths': {
    name: 'Ocean Depths',
    bg1: '#001F3F',
    bg2: '#003366',
    bg3: '#00447A',
    bg4: '#004C8C',
    bg5: '#0066B3',
    bg6: '#0080D9',
    bg7: '#00A0E0',
    fg: '#FFFFFF',
    fgAccent: '#87CEEB',
    multiStopGradient: true,
  },
  'Aurora Borealis': {
    name: 'Aurora Borealis',
    bg1: '#020B1A',
    bg2: '#051832',
    bg3: '#082E46',
    bg4: '#0C4550',
    bg5: '#105E54',
    bg6: '#1C8B62',
    bg7: '#2ECC71',
    fg: '#E0FFF4',
    fgAccent: '#7DFFC3',
    multiStopGradient: true,
  },
  'Sunset Glow': {
    name: 'Sunset Glow',
    bg1: '#4A1C1C',
    bg2: '#6B2D2D',
    bg3: '#7E3636',
    bg4: '#8C3E3E',
    bg5: '#AD4F4F',
    bg6: '#CE6060',
    bg7: '#E07020',
    fg: '#FFFFFF',
    fgAccent: '#FFD700',
  },
  'Solar Flare': {
    name: 'Solar Flare',
    bg1: '#1A0800',
    bg2: '#331400',
    bg3: '#4D2200',
    bg4: '#6B3500',
    bg5: '#884C00',
    bg6: '#B86B00',
    bg7: '#FF8C00',
    fg: '#FFFFFF',
    fgAccent: '#FFD700',
    multiStopGradient: true,
  },
  'Glacier': {
    name: 'Glacier',
    bg1: '#06111C',
    bg2: '#0E2240',
    bg3: '#163560',
    bg4: '#224D7E',
    bg5: '#35689A',
    bg6: '#4A8AB5',
    bg7: '#7EC8E3',
    fg: '#F0F8FF',
    fgAccent: '#B4F0FF',
  },
  'Rose Gold': {
    name: 'Rose Gold',
    bg1: '#3D1F2B',
    bg2: '#5C2E41',
    bg3: '#6C364C',
    bg4: '#7B3D57',
    bg5: '#9A4C6D',
    bg6: '#B95B83',
    bg7: '#D86A99',
    fg: '#FFFFFF',
    fgAccent: '#FFB6C1',
  },
  'Cyberpunk': {
    name: 'Cyberpunk',
    bg1: '#0D0221',
    bg2: '#1A0A3E',
    bg3: '#240E4C',
    bg4: '#2D1259',
    bg5: '#541388',
    bg6: '#7B1FA2',
    bg7: '#FF006E',
    fg: '#00FFFF',
    fgAccent: '#FF00FF',
    multiStopGradient: true,
  },
  'Monochrome Dark': {
    name: 'Monochrome Dark',
    bg1: '#000000',
    bg2: '#1A1A1A',
    bg3: '#2D2D2D',
    bg4: '#404040',
    bg5: '#555555',
    bg6: '#6A6A6A',
    bg7: '#808080',
    fg: '#FFFFFF',
    fgAccent: '#CCCCCC',
  },
  'Monochrome Light': {
    name: 'Monochrome Light',
    bg1: '#FFFFFF',
    bg2: '#F0F0F0',
    bg3: '#E0E0E0',
    bg4: '#D0D0D0',
    bg5: '#C0C0C0',
    bg6: '#B0B0B0',
    bg7: '#A0A0A0',
    fg: '#000000',
    fgAccent: '#333333',
  },
  'Neon Tokyo': {
    name: 'Neon Tokyo',
    bg1: '#080011',
    bg2: '#140828',
    bg3: '#281044',
    bg4: '#421860',
    bg5: '#661E74',
    bg6: '#AA1E6E',
    bg7: '#FF2975',
    fg: '#00FFE5',
    fgAccent: '#FFE54C',
    multiStopGradient: true,
  },
  'Dracula': {
    name: 'Dracula',
    bg1: '#21222C',
    bg2: '#282A36',
    bg3: '#2E303E',
    bg4: '#343746',
    bg5: '#44475A',
    bg6: '#6272A4',
    bg7: '#BD93F9',
    fg: '#F8F8F2',
    fgAccent: '#FF79C6',
    multiStopGradient: true,
  },
  'Matrix': {
    name: 'Matrix',
    bg1: '#000000',
    bg2: '#001500',
    bg3: '#002A00',
    bg4: '#004200',
    bg5: '#005E00',
    bg6: '#008B00',
    bg7: '#00FF41',
    fg: '#00FF41',
    fgAccent: '#33FF77',
    multiStopGradient: true,
  },
  'Vaporwave': {
    name: 'Vaporwave',
    bg1: '#0A1520',
    bg2: '#1A1E30',
    bg3: '#2E2440',
    bg4: '#442A52',
    bg5: '#603066',
    bg6: '#983878',
    bg7: '#FF71CE',
    fg: '#00FFD4',
    fgAccent: '#01CDFE',
    multiStopGradient: true,
  },
  'Ember': {
    name: 'Ember',
    bg1: '#1A0A00',
    bg2: '#2E1400',
    bg3: '#441E08',
    bg4: '#5C2A10',
    bg5: '#763818',
    bg6: '#994520',
    bg7: '#D4760A',
    fg: '#FFF5E6',
    fgAccent: '#FFB84D',
  },
  'Electric Lime': {
    name: 'Electric Lime',
    bg1: '#050A00',
    bg2: '#101C05',
    bg3: '#1E3008',
    bg4: '#2E4610',
    bg5: '#406018',
    bg6: '#588020',
    bg7: '#84CC16',
    fg: '#F0FFF0',
    fgAccent: '#BEFF5A',
  },
  'Tidal': {
    name: 'Tidal',
    bg1: '#020A18',
    bg2: '#061530',
    bg3: '#0A2848',
    bg4: '#104060',
    bg5: '#186078',
    bg6: '#208890',
    bg7: '#2DD4BF',
    fg: '#E0FFFF',
    fgAccent: '#48D1CC',
    multiStopGradient: true,
  },
  'Amber': {
    name: 'Amber',
    bg1: '#0E0A00',
    bg2: '#1E1800',
    bg3: '#302800',
    bg4: '#483C00',
    bg5: '#665200',
    bg6: '#887000',
    bg7: '#BFA200',
    fg: '#FFFDD0',
    fgAccent: '#FFD700',
    multiStopGradient: true,
  },
  'Synthwave': {
    name: 'Synthwave',
    bg1: '#1A1A2E',
    bg2: '#262640',
    bg3: '#2C2C49',
    bg4: '#323252',
    bg5: '#4A3F6B',
    bg6: '#614385',
    bg7: '#FF2E97',
    fg: '#FFFFFF',
    fgAccent: '#00F3FF',
    multiStopGradient: true,
  },
  'Molten': {
    name: 'Molten',
    bg1: '#100004',
    bg2: '#200810',
    bg3: '#381018',
    bg4: '#501820',
    bg5: '#702020',
    bg6: '#983028',
    bg7: '#b83020',
    fg: '#FFD8C8',
    fgAccent: '#FF6840',
    multiStopGradient: true,
  },
  'Rainbow': {
    name: 'Rainbow',
    bg1: '#8B0000',  // Deep red
    bg2: '#8B4500',  // Dark orange
    bg3: '#6B6B00',  // Dark yellow
    bg4: '#006400',  // Dark green
    bg5: '#00008B',  // Dark blue
    bg6: '#4B0082',  // Indigo
    bg7: '#800080',  // Purple
    fg: '#FFFFFF',
    fgAccent: '#FFD700',
    multiStopGradient: true,
  },
  'Delulus Club': {
    name: 'Delulus Club',
    bg1: '#2D1864',  // deep indigo
    bg2: '#4A26A8',  // violet-ink (dark purple)
    bg3: '#6B3FD6',  // brand violet
    bg4: '#3BC43A',  // brand green — dominant from here
    bg5: '#3BC43A',
    bg6: '#3BC43A',
    bg7: '#3BC43A',
    fg: '#FBF7F0',
    fgAccent: '#F4C21E',
    multiStopGradient: true,
  },
};

/**
 * Get a theme by name, falling back to Purple Haze if not found
 */
export function getTheme(name: string): Theme {
  return themes[name] || themes['Purple Haze'];
}

/**
 * Generate the hardstatus line for a given theme
 * Layout: [project] [/] [title] %= [AI stats] %= [Last:] [time] [·] [ImmorTerm]
 * Note: %I = last I/O activity timestamp (ImmorTerm C code feature - zero polling!)
 * Note: %Z = AI stats pushed via OSC 777 from VS Code extension (event-driven, zero polling!)
 *
 * ImmorTerm gradient mode:
 * - %{G#RRGGBB#RRGGBB[#RRGGBB...]} enables multi-stop gradient background
 * - Each character gets an interpolated background color between adjacent stops
 * - Foreground colors and attributes apply on top of the gradient
 */
export function generateHardstatus(theme: Theme): string {
  // Note: Single quotes for screen hardstatus format string (shell quoting)
  // %{G#stop1#stop2...#stopN} enables gradient background across the entire status line
  // Most themes use 2 stops (bg1→bg6), multi-hue themes use all 7 (bg1→bg7)
  // %{-} at end pops rendition stack for clean state
  // AI stats use orange foreground (#FFA500) via %Z (OSC 777, event-driven, zero polling)
  // %?%Z %? = conditional: only shows AI stats + trailing space if non-empty
  // AI stats are centered between two %= pads (left-pad, stats, right-pad)
  // %?%K %? = conditional: shows 🔒 lock indicator after title when IMMORTERM_TITLE_LOCKED=1 (K for locK; L is reserved as long-format modifier)
  // IMPORTANT: Space before %= is required! pad_expand() has a bug where if a rendition
  // position is at the exact same buffer position as CHRPAD, it never gets adjusted
  // during padding expansion, causing padding spaces to inherit wrong colors.
  const gradient = theme.multiStopGradient
    ? `G${theme.bg1}${theme.bg2}${theme.bg3}${theme.bg4}${theme.bg5}${theme.bg6}${theme.bg7}`
    : `G${theme.bg1}${theme.bg6}`;
  // %{S#RRGGBB} = shimmer escape: sets base fg color AND enables spotlight animation
  // The shimmer sweeps across "ImmorTerm" text every 30 seconds
  return `'%{${gradient}}%{= ${theme.fg}}  %2\`%{= ${theme.fg}} /%{= ${theme.fg}} %t %?%K%?%=%{= #FFA500}%?%Z%? %=%{= ${theme.fg}} Last:%{= ${theme.fg}} %I%{= ${theme.fgAccent}} %J%{S${theme.fgAccent}} ImmorTerm  %{-}'`;
}

/**
 * List all available theme names
 */
export function getThemeNames(): string[] {
  return Object.keys(themes);
}

/**
 * Theme display labels with emojis for UI pickers
 */
export const themeLabels: Record<string, string> = {
  'Purple Haze': '🟣 Purple Haze',
  'Ocean Depths': '🔵 Ocean Depths',
  'Aurora Borealis': '🌌 Aurora Borealis',
  'Sunset Glow': '🟠 Sunset Glow',
  'Solar Flare': '☀️ Solar Flare',
  'Glacier': '🏔️ Glacier',
  'Rose Gold': '🩷 Rose Gold',
  'Cyberpunk': '💜 Cyberpunk',
  'Monochrome Dark': '⬛ Monochrome Dark',
  'Monochrome Light': '⬜ Monochrome Light',
  'Neon Tokyo': '🏙️ Neon Tokyo',
  'Dracula': '🧛 Dracula',
  'Matrix': '💊 Matrix',
  'Vaporwave': '📼 Vaporwave',
  'Ember': '🔥 Ember',
  'Electric Lime': '⚡ Electric Lime',
  'Tidal': '🌊 Tidal',
  'Amber': '🟡 Amber',
  'Synthwave': '🌆 Synthwave',
  'Molten': '🌋 Molten',
  'Rainbow': '🌈 Rainbow',
  'Delulus Club': '🌱 Delulus Club',
};

/**
 * Get the display label for a theme (with emoji)
 */
export function getThemeLabel(name: string): string {
  return themeLabels[name] || name;
}
