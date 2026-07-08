/**
 * Theme-aware hibernation message for Claude session reaper.
 * Builds a truecolor ANSI ASCII art message with multi-stop gradient
 * coloring — the same interpolation algorithm as ImmorTerm's C code
 * (screen.c:interpolate_gradient_color).
 */

import { Theme, getTheme } from './themes';

// ── ANSI helpers ──────────────────────────────────────────────

const RESET = '\x1b[0m';
const BOLD = '\x1b[1m';
const DIM = '\x1b[2m';

function fgAnsi(r: number, g: number, b: number): string {
    return `\x1b[38;2;${r};${g};${b}m`;
}

function hexToRgb(hex: string): [number, number, number] {
    return [
        parseInt(hex.slice(1, 3), 16),
        parseInt(hex.slice(3, 5), 16),
        parseInt(hex.slice(5, 7), 16),
    ];
}

function hexToAnsi(hex: string): string {
    const [r, g, b] = hexToRgb(hex);
    return fgAnsi(r, g, b);
}

// ── Gradient engine (ported from C: screen.c:interpolate_gradient_color) ──

/**
 * Brighten a color by scaling up its channels to ensure visibility.
 * Unlike lerping toward white (which desaturates to pastel), this
 * multiplies all channels so the max channel reaches the target.
 * Result: dark red #8B0000 → vivid red (200,0,0), not pastel pink.
 */
function brightenRgb(
    rgb: [number, number, number],
    minBrightness = 200,
): [number, number, number] {
    const maxCh = Math.max(...rgb);
    if (maxCh >= minBrightness) return rgb; // already bright
    if (maxCh === 0) return [minBrightness, minBrightness, minBrightness];
    const scale = minBrightness / maxCh;
    return rgb.map(c => Math.min(255, Math.round(c * scale))) as [number, number, number];
}

/**
 * Build bright gradient stops for the ASCII art logo.
 *
 * multiStopGradient themes (Rainbow, Cyberpunk, Synthwave, etc.) have 7
 * distinct hues in bg1→bg7 — those are designed as dark BACKGROUND colors
 * for the status bar. We brighten each one so they're visible as foreground
 * text on a dark terminal, preserving the full multi-hue gradient.
 *
 * Regular themes only have a monotonic dark→light ramp in bg1→bg6, so
 * brightening them just produces a washed-out gradient. For those, we
 * use the theme's vibrant accent colors: fgAccent → bg7 → fg.
 */
function getLogoGradientStops(theme: Theme): Array<[number, number, number]> {
    if (theme.multiStopGradient) {
        return [theme.bg1, theme.bg2, theme.bg3, theme.bg4, theme.bg5, theme.bg6, theme.bg7]
            .map(hexToRgb)
            .map(c => brightenRgb(c));
    }
    return [theme.fgAccent, theme.bg7, theme.fg].map(hexToRgb);
}

/**
 * Build a border gradient — uses the brighter half of the bg spectrum
 * (bg5 → bg6 → bg7) brightened for visibility.
 */
function getBorderGradientStops(theme: Theme): Array<[number, number, number]> {
    return [theme.bg5, theme.bg6, theme.bg7]
        .map(hexToRgb)
        .map(c => brightenRgb(c));
}

/**
 * Interpolate a color from multi-stop gradient — exact port of C code.
 * @param stops  Array of [r,g,b] color stops
 * @param position  Character position (0-based)
 * @param totalWidth  Total number of characters in the row
 * @param offset  Optional shift (for diagonal/wave effects), in [0,1]
 */
function interpolateGradient(
    stops: Array<[number, number, number]>,
    position: number,
    totalWidth: number,
    offset = 0,
): [number, number, number] {
    const n = stops.length;
    if (n < 2) return stops[0] || [255, 255, 255];
    if (totalWidth <= 1) return stops[0];

    // Map position to [0, n-1] — matching C code exactly
    let t = (position / totalWidth) * (n - 1);

    // Apply offset with palindrome reflection (from C code)
    // This prevents jarring color cliffs on monotonic themes
    if (offset !== 0) {
        t += offset * (n - 1);
        const span = n - 1;
        const period = 2 * span;
        t = ((t % period) + period) % period; // fmod that handles negatives
        if (t > span) t = period - t;
    }

    const seg = Math.min(Math.floor(t), n - 2);
    const frac = t - seg;

    // Linear interpolation within segment
    const [r1, g1, b1] = stops[seg];
    const [r2, g2, b2] = stops[seg + 1];

    return [
        Math.max(0, Math.min(255, Math.round(r1 + (r2 - r1) * frac))),
        Math.max(0, Math.min(255, Math.round(g1 + (g2 - g1) * frac))),
        Math.max(0, Math.min(255, Math.round(b1 + (b2 - b1) * frac))),
    ];
}

// ── Text utilities ────────────────────────────────────────────

/** Strip ANSI escape codes to get visible character count */
function visibleWidth(s: string): number {
    return s.replace(/\x1b\[[0-9;]*m/g, '').length;
}

/** Right-pad plain text to a fixed width */
function pad(s: string, width: number): string {
    return s.length >= width ? s : s + ' '.repeat(width - s.length);
}

/**
 * Apply a horizontal gradient to a text string.
 * Non-space characters get interpolated foreground color;
 * spaces stay uncolored for transparent background.
 * @param rowOffset  Diagonal offset [0,1] — shifts the gradient per row
 */
function gradientText(
    text: string,
    stops: Array<[number, number, number]>,
    rowOffset = 0,
): string {
    const width = text.length;
    let result = '';
    let lastAnsi = '';

    for (let i = 0; i < width; i++) {
        const ch = text[i];
        if (ch === ' ') {
            result += ' ';
            lastAnsi = '';
        } else {
            const [r, g, b] = interpolateGradient(stops, i, width, rowOffset);
            const ansi = fgAnsi(r, g, b);
            // Only emit escape if color changed (reduces output size)
            if (ansi !== lastAnsi) {
                result += ansi;
                lastAnsi = ansi;
            }
            result += ch;
        }
    }
    return result + RESET;
}

// ── The IMMORTERM ASCII art ───────────────────────────────────

const ART = [
    '  _____ __  __ __  __  ____  _____ _______ ______ _____  __  __ ',
    ' |_   _|  \\╱  |  \\╱  |╱ __ \\|  __ \\__   __|  ____|  __ \\|  \\╱  |',
    '   | | | \\  ╱ | \\  ╱ | |  | | |__) | | |  | |__  | |__) | \\  ╱ |',
    '   | | | |\\╱| | |\\╱| | |  | |  _  ╱  | |  |  __| |  _  ╱| |\\╱| |',
    '  _| |_| |  | | |  | | |__| | | \\ \\  | |  | |____| | \\ \\| |  | |',
    ' |_____|_|  |_|_|  |_|\\____╱|_|  \\_\\ |_|  |______|_|  \\_\\_|  |_|',
];

// ── Main builder ──────────────────────────────────────────────

export function buildHibernationMessage(
    themeName: string,
    stats: {
        rssMB: number;
        idleHours: number;
        idleMinutes: number;
        lastActivity: string;  // ISO timestamp
        sessionUuid: string;
        terminalName: string;
    }
): string {
    const theme = getTheme(themeName);
    const logoStops = getLogoGradientStops(theme);
    const borderStops = getBorderGradientStops(theme);

    // Determine box width from ASCII art
    const artWidth = Math.max(...ART.map(l => l.length));
    const W = artWidth + 6; // 3 left margin + 3 right padding

    // Color shortcuts for non-gradient elements
    const border = hexToAnsi(theme.bg5);
    const title = hexToAnsi(theme.bg7);
    const text = hexToAnsi(theme.fg);
    const labelClr = hexToAnsi(theme.fgAccent);
    const statsBox = hexToAnsi(theme.bg6);
    const dimClr = hexToAnsi(theme.bg6);
    const cmd = hexToAnsi(theme.fg);
    const prompt = hexToAnsi(theme.bg7);

    // Auto-padded bordered line
    function bline(content: string): string {
        const padN = Math.max(0, W - visibleWidth(content));
        return `  ${border}│${RESET}${content}${' '.repeat(padN)}${border}│${RESET}`;
    }

    // Border line with per-character gradient (for top/bottom frame)
    function gradBorderH(left: string, fill: string, right: string): string {
        const fillCount = W;
        let result = `  `;
        for (let i = 0; i < fillCount + 2; i++) {
            const ch = i === 0 ? left : i === fillCount + 1 ? right : fill;
            const [r, g, b] = interpolateGradient(borderStops, i, fillCount + 2);
            result += fgAnsi(r, g, b) + ch;
        }
        return result + RESET;
    }

    const empty = bline('');

    // Format values
    const idleStr = stats.idleHours > 0
        ? `${stats.idleHours}h ${stats.idleMinutes}m`
        : `${stats.idleMinutes}m`;

    const lastAct = stats.lastActivity
        .replace(/:\d{2}\.\d{3}Z$/, ' UTC')
        .replace('T', ' ');

    const memStr = stats.rssMB >= 1000
        ? `${(stats.rssMB / 1024).toFixed(1)} GB`
        : `${stats.rssMB.toLocaleString()} MB`;

    const resumeCmd = `claude --resume ${stats.sessionUuid}`;

    // Stats box
    const SB = 45;
    function sline(lbl: string, val: string): string {
        const lblPad = pad(lbl, 16);
        const valPad = pad(val, SB - 16 - 4);
        return bline(`   ${statsBox}│${RESET}  ${DIM}${labelClr}${lblPad}${RESET}${BOLD}${text}${valPad}${RESET}${statsBox}│${RESET}`);
    }

    // Build the gradient ASCII art lines with diagonal sweep
    // Each row gets a slight offset (0.08 per row) creating a wave effect
    const artLines = ART.map((line, row) => {
        const padded = pad(line, artWidth); // normalize width
        const colored = gradientText(padded, logoStops, row * 0.08);
        return bline(`   ${BOLD}${colored}`);
    });

    const lines = [
        '',
        gradBorderH('╭', '─', '╮'),
        empty,

        // Gradient ASCII art logo
        ...artLines,

        empty,

        // Title
        bline(`   ${BOLD}${title}⏸  Session Hibernated${RESET}`),

        empty,

        // Explanation
        bline(`   ${dimClr}This Claude session was idle for ${idleStr} with no${RESET}`),
        bline(`   ${dimClr}terminal I/O and was suspended to free resources.${RESET}`),

        empty,

        // Stats box
        bline(`   ${statsBox}┌${'─'.repeat(SB - 2)}┐${RESET}`),
        sline('Memory freed', memStr),
        sline('Idle duration', idleStr),
        sline('Last activity', lastAct),
        bline(`   ${statsBox}└${'─'.repeat(SB - 2)}┘${RESET}`),

        empty,

        // Resume instruction
        bline(`   ${text}Resume anytime:${RESET}`),
        bline(`   ${BOLD}${prompt}$ ${cmd}${resumeCmd}${RESET}`),

        empty,
        gradBorderH('╰', '─', '╯'),
        '',
    ];

    return lines.join('\n');
}
