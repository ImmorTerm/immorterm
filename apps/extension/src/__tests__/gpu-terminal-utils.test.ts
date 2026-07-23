// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest';

// Import directly from the JS module — Vitest/jsdom handles ES modules fine
import {
  formatMemory,
  formatRuntime,
  contextBar,
  hexToRgb,
  parseCSSColor,
  parseCSSColorWithAlpha,
  stripEmojis,
  el,
  clearEl,
  createReportError,
  createRenderSidebar,
  normalizeSpaceRecord,
} from '../../resources/gpu-terminal-utils.js';

// ── Pure Function Tests ──────────────────────────────────────────

describe('formatMemory', () => {
  it('formats kilobytes as megabytes', () => {
    expect(formatMemory(1024)).toBe('1M');
  });

  it('formats large KB values as megabytes', () => {
    expect(formatMemory(512000)).toBe('500M');
  });

  it('formats gigabyte values', () => {
    expect(formatMemory(1048576)).toBe('1.0G');
  });

  it('formats fractional gigabytes', () => {
    expect(formatMemory(2621440)).toBe('2.5G');
  });

  it('rounds small values', () => {
    expect(formatMemory(100)).toBe('0M');
  });
});

describe('formatRuntime', () => {
  it('formats minutes and seconds', () => {
    expect(formatRuntime(125)).toBe('2m5s');
  });

  it('formats hours and minutes', () => {
    expect(formatRuntime(3661)).toBe('1h1m');
  });

  it('formats exact hours', () => {
    expect(formatRuntime(7200)).toBe('2h');
  });

  it('formats zero seconds in minutes range', () => {
    expect(formatRuntime(120)).toBe('2m');
  });

  it('formats seconds only', () => {
    expect(formatRuntime(45)).toBe('0m45s');
  });
});

describe('contextBar', () => {
  it('returns empty bar at 0%', () => {
    expect(contextBar(0)).toBe('\u25B1'.repeat(10));
  });

  it('returns full bar at 100%', () => {
    expect(contextBar(100)).toBe('\u25B0'.repeat(10));
  });

  it('returns half bar at 50%', () => {
    const result = contextBar(50);
    expect(result).toBe('\u25B0'.repeat(5) + '\u25B1'.repeat(5));
  });

  it('handles null/undefined', () => {
    expect(contextBar(null)).toBe('\u25B1'.repeat(10));
    expect(contextBar(undefined)).toBe('\u25B1'.repeat(10));
  });

  it('rounds to nearest 10%', () => {
    // 25% → rounds to 3 filled (Math.round(2.5) = 3)
    const result = contextBar(25);
    expect(result.length).toBe(10);
  });
});

describe('hexToRgb', () => {
  it('parses red', () => {
    expect(hexToRgb('#ff0000')).toEqual({ r: 255, g: 0, b: 0 });
  });

  it('parses green', () => {
    expect(hexToRgb('#00ff00')).toEqual({ r: 0, g: 255, b: 0 });
  });

  it('parses white', () => {
    expect(hexToRgb('#ffffff')).toEqual({ r: 255, g: 255, b: 255 });
  });

  it('parses black', () => {
    expect(hexToRgb('#000000')).toEqual({ r: 0, g: 0, b: 0 });
  });

  it('parses mixed color', () => {
    expect(hexToRgb('#1e1e2e')).toEqual({ r: 30, g: 30, b: 46 });
  });
});

describe('parseCSSColor', () => {
  it('parses #RRGGBB hex', () => {
    const result = parseCSSColor('#ff0000');
    expect(result).toEqual([1.0, 0, 0]);
  });

  it('parses #RGB shorthand', () => {
    const result = parseCSSColor('#f00');
    expect(result).toEqual([1.0, 0, 0]);
  });

  it('parses #RRGGBBAA (strips alpha)', () => {
    const result = parseCSSColor('#ff000080');
    expect(result).toEqual([1.0, 0, 0]);
  });

  it('parses rgb() notation', () => {
    const result = parseCSSColor('rgb(255, 0, 0)');
    expect(result).toEqual([1.0, 0, 0]);
  });

  it('parses rgba() notation (returns RGB only)', () => {
    const result = parseCSSColor('rgba(255, 128, 0, 0.5)');
    expect(result![0]).toBeCloseTo(1.0);
    expect(result![1]).toBeCloseTo(128 / 255);
    expect(result![2]).toBe(0);
    expect(result!.length).toBe(3);
  });

  it('returns null for empty/invalid input', () => {
    expect(parseCSSColor('')).toBeNull();
    expect(parseCSSColor(null)).toBeNull();
    expect(parseCSSColor(undefined)).toBeNull();
    expect(parseCSSColor('invalid')).toBeNull();
  });

  it('handles whitespace', () => {
    expect(parseCSSColor('  #ff0000  ')).toEqual([1.0, 0, 0]);
  });
});

describe('parseCSSColorWithAlpha', () => {
  it('parses #RRGGBB (adds alpha 1.0)', () => {
    const result = parseCSSColorWithAlpha('#ff0000');
    expect(result).toEqual([1.0, 0, 0, 1.0]);
  });

  it('parses #RRGGBBAA', () => {
    const result = parseCSSColorWithAlpha('#ff000080');
    expect(result![0]).toBeCloseTo(1.0);
    expect(result![3]).toBeCloseTo(128 / 255);
  });

  it('parses #RGB (expands to RRGGBBFF)', () => {
    const result = parseCSSColorWithAlpha('#f00');
    expect(result).toEqual([1.0, 0, 0, 1.0]);
  });

  it('parses rgba() with alpha', () => {
    const result = parseCSSColorWithAlpha('rgba(255, 0, 0, 0.5)');
    expect(result).toEqual([1.0, 0, 0, 0.5]);
  });

  it('parses rgb() (default alpha 1.0)', () => {
    const result = parseCSSColorWithAlpha('rgb(255, 0, 0)');
    expect(result).toEqual([1.0, 0, 0, 1.0]);
  });

  it('returns null for invalid', () => {
    expect(parseCSSColorWithAlpha('')).toBeNull();
    expect(parseCSSColorWithAlpha(null)).toBeNull();
  });
});

describe('stripEmojis', () => {
  it('strips emojis and replaces with double-width spaces', () => {
    const result = stripEmojis('hello \u{1F600} world');
    expect(result.clean).toBe('hello    world');
    expect(result.emojis).toHaveLength(1);
    expect(result.emojis[0].char).toBe('\u{1F600}');
  });

  it('returns clean string when no emojis', () => {
    const result = stripEmojis('hello world');
    expect(result.clean).toBe('hello world');
    expect(result.emojis).toHaveLength(0);
  });

  it('tracks column positions correctly', () => {
    const result = stripEmojis('A\u{2600}B');
    // A is col 0, emoji at col 1 (replaced with 2 spaces), B at col 3
    expect(result.emojis[0].col).toBe(1);
    expect(result.clean).toBe('A  B');
  });

  it('handles multiple emojis', () => {
    const result = stripEmojis('\u{1F300}\u{1F301}');
    expect(result.emojis).toHaveLength(2);
    expect(result.clean).toBe('    ');
  });
});

// ── DOM Utility Tests ────────────────────────────────────────────

describe('el', () => {
  it('creates element with tag', () => {
    const node = el('div');
    expect(node.tagName).toBe('DIV');
  });

  it('sets class name', () => {
    const node = el('span', 'my-class');
    expect(node.className).toBe('my-class');
  });

  it('sets text content', () => {
    const node = el('p', '', 'hello');
    expect(node.textContent).toBe('hello');
  });

  it('omits class when null', () => {
    const node = el('div', null, 'text');
    expect(node.className).toBe('');
    expect(node.textContent).toBe('text');
  });
});

describe('clearEl', () => {
  it('removes all children', () => {
    const parent = document.createElement('div');
    parent.appendChild(document.createElement('span'));
    parent.appendChild(document.createElement('span'));
    expect(parent.childNodes.length).toBe(2);

    clearEl(parent);
    expect(parent.childNodes.length).toBe(0);
  });
});

// ── Error Toast Tests ────────────────────────────────────────────

describe('createReportError', () => {
  let container: HTMLDivElement;
  let mockPostMessage: ReturnType<typeof vi.fn>;
  let reportError: ReturnType<typeof createReportError>;

  beforeEach(() => {
    container = document.createElement('div');
    document.body.appendChild(container);
    mockPostMessage = vi.fn();
    reportError = createReportError({
      postMessage: mockPostMessage,
      appendTo: container,
      autoDismissMs: 100, // fast for tests
    });
  });

  it('creates a toast element', () => {
    reportError('test', new Error('boom'));
    const toasts = container.querySelectorAll('div');
    expect(toasts.length).toBe(1);
    expect(toasts[0].textContent).toContain('[test] boom');
  });

  it('sends error message to extension', () => {
    const err = new Error('test error');
    reportError('source', err);
    expect(mockPostMessage).toHaveBeenCalledWith({
      type: 'error',
      message: '[source] test error',
      stack: err.stack,
    });
  });

  it('toast has a close button that removes it', () => {
    reportError('test', 'err');
    const toast = container.querySelector('div')!;
    const closeBtn = toast.querySelector('button')!;
    expect(closeBtn).toBeTruthy();
    expect(closeBtn.textContent).toBe('\u00d7');

    closeBtn.click();
    expect(container.querySelector('div')).toBeNull();
  });

  it('auto-removes after timeout', () => {
    vi.useFakeTimers();
    reportError('test', 'err');
    expect(container.querySelector('div')).toBeTruthy();

    vi.advanceTimersByTime(100);
    expect(container.querySelector('div')).toBeNull();
    vi.useRealTimers();
  });

  it('creates separate toasts for multiple errors', () => {
    reportError('a', 'error 1');
    reportError('b', 'error 2');
    const toasts = container.querySelectorAll('div');
    expect(toasts.length).toBe(2);
  });

  it('handles string errors (no .message property)', () => {
    reportError('test', 'string error');
    const toast = container.querySelector('div')!;
    expect(toast.textContent).toContain('[test] string error');
  });
});

// ── Sidebar Rendering Tests ──────────────────────────────────────

describe('createRenderSidebar', () => {
  let sessionListEl: HTMLDivElement;
  let sessions: Map<string, any>;
  let onSwitch: ReturnType<typeof vi.fn>;
  let onRename: ReturnType<typeof vi.fn>;
  let onRemove: ReturnType<typeof vi.fn>;
  let onContextMenu: ReturnType<typeof vi.fn>;
  let renderSidebar: ReturnType<typeof createRenderSidebar>;

  beforeEach(() => {
    sessionListEl = document.createElement('div');
    sessionListEl.id = 'session-list';
    document.body.appendChild(sessionListEl);

    sessions = new Map();
    onSwitch = vi.fn();
    onRename = vi.fn();
    onRemove = vi.fn();
    onContextMenu = vi.fn();

    renderSidebar = createRenderSidebar({
      sessionListEl,
      sessions,
      getActiveSessionName: () => 'session-1',
      onSwitch,
      onRename,
      onRemove,
      onContextMenu,
    });
  });

  it('renders empty list when no sessions', () => {
    renderSidebar();
    expect(sessionListEl.querySelectorAll('.session-item').length).toBe(0);
  });

  it('renders session items', () => {
    sessions.set('session-1', { connected: true, displayName: 'My Session' });
    sessions.set('session-2', { connected: false, displayName: 'Other' });
    renderSidebar();

    expect(sessionListEl.querySelectorAll('.session-item').length).toBe(2);
  });

  it('marks active session with .active class', () => {
    sessions.set('session-1', { connected: true, displayName: 'Active' });
    sessions.set('session-2', { connected: true, displayName: 'Inactive' });
    renderSidebar();

    const items = sessionListEl.querySelectorAll('.session-item');
    expect(items[0].classList.contains('active')).toBe(true);
    expect(items[1].classList.contains('active')).toBe(false);
  });

  it('shows disconnected dot for disconnected sessions', () => {
    sessions.set('session-1', { connected: false, displayName: 'Disc' });
    renderSidebar();

    const dot = sessionListEl.querySelector('.dot')!;
    expect(dot.classList.contains('disconnected')).toBe(true);
  });

  it('calls onRemove when close button is clicked', () => {
    sessions.set('session-1', { connected: true, displayName: 'Test' });
    renderSidebar();

    const closeBtn = sessionListEl.querySelector('.close') as HTMLButtonElement;
    closeBtn.click();
    expect(onRemove).toHaveBeenCalledWith('session-1');
  });

  it('calls onSwitch when session item is clicked', () => {
    sessions.set('session-1', { connected: true, displayName: 'Test' });
    renderSidebar();

    const item = sessionListEl.querySelector('.session-item') as HTMLDivElement;
    item.click();
    expect(onSwitch).toHaveBeenCalledWith('session-1');
  });

  it('calls onContextMenu on right-click', () => {
    sessions.set('session-1', { connected: true, displayName: 'Test' });
    renderSidebar();

    const item = sessionListEl.querySelector('.session-item') as HTMLDivElement;
    const event = new MouseEvent('contextmenu', { bubbles: true });
    item.dispatchEvent(event);
    expect(onContextMenu).toHaveBeenCalledWith(event, 'session-1');
  });

  it('calls onRename on double-click name', () => {
    const session = { connected: true, displayName: 'Test' };
    sessions.set('session-1', session);
    renderSidebar();

    const nameEl = sessionListEl.querySelector('.name') as HTMLSpanElement;
    nameEl.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    expect(onRename).toHaveBeenCalledWith('session-1', session, nameEl);
  });

  it('clears and rebuilds on each call', () => {
    sessions.set('session-1', { connected: true, displayName: 'First' });
    renderSidebar();
    expect(sessionListEl.querySelectorAll('.session-item').length).toBe(1);

    sessions.set('session-2', { connected: true, displayName: 'Second' });
    renderSidebar();
    expect(sessionListEl.querySelectorAll('.session-item').length).toBe(2);
  });

  it('shows drawing badge for sessions with AI primitives', () => {
    sessions.set('session-1', { connected: true, displayName: 'Art', aiPrimitiveCount: 3 });
    renderSidebar();

    const badge = sessionListEl.querySelector('.drawing-badge');
    expect(badge).toBeTruthy();
    expect(badge!.getAttribute('title')).toBe('3 drawings');
  });

  it('does not show drawing badge when count is 0', () => {
    sessions.set('session-1', { connected: true, displayName: 'No Art', aiPrimitiveCount: 0 });
    renderSidebar();

    expect(sessionListEl.querySelector('.drawing-badge')).toBeNull();
  });

  // ── Reentrant Guard Tests ──

  it('reentrant call is a no-op (guard prevents crash)', () => {
    let callCount = 0;
    const originalOnSwitch = onSwitch;

    // Set up a scenario where rendering triggers itself
    // (simulates blur event during DOM teardown calling renderSidebar)
    sessions.set('session-1', { connected: true, displayName: 'Test' });

    // Override onSwitch to trigger renderSidebar inside itself
    const guardedRender = createRenderSidebar({
      sessionListEl,
      sessions,
      getActiveSessionName: () => 'session-1',
      onSwitch: () => {
        callCount++;
        // Try to call renderSidebar while it's already rendering
        // This should be silently ignored (no-op)
        guardedRender();
      },
      onRename,
      onRemove,
      onContextMenu,
    });

    // First render works
    guardedRender();
    // Click triggers onSwitch which calls guardedRender again (reentrant)
    const item = sessionListEl.querySelector('.session-item') as HTMLDivElement;
    item.click();

    // onSwitch was called (so the click handler worked)
    expect(callCount).toBe(1);
    // No error thrown — guard worked
  });

  // ── Regression Test: removeChild bug during rename ──

  it('regression: renderSidebar during active rename does not throw', () => {
    sessions.set('session-1', { connected: true, displayName: 'Original' });
    renderSidebar();

    // Simulate rename flow: replace the name span with an input
    const nameSpan = sessionListEl.querySelector('.name') as HTMLSpanElement;
    const input = document.createElement('input');
    input.className = 'name';
    input.value = 'New Name';
    nameSpan.replaceWith(input);
    input.focus();

    // Now trigger renderSidebar (e.g. from an update-display-name message)
    // Previously this would crash because blur fires during DOM teardown.
    // The reentrant guard and textContent clearing prevent this.
    expect(() => renderSidebar()).not.toThrow();
  });

  // ── isRenaming guard tests ──

  it('skips re-render when isRenaming returns true', () => {
    let renaming = false;
    const guardedRender = createRenderSidebar({
      sessionListEl,
      sessions,
      getActiveSessionName: () => 'session-1',
      onSwitch,
      onRename,
      onRemove,
      onContextMenu,
      isRenaming: () => renaming,
    });

    sessions.set('session-1', { connected: true, displayName: 'First' });
    guardedRender();
    expect(sessionListEl.querySelectorAll('.session-item').length).toBe(1);

    // Start "renaming"
    renaming = true;
    sessions.set('session-2', { connected: true, displayName: 'Second' });
    guardedRender();
    // Sidebar should NOT have been rebuilt — still 1 item
    expect(sessionListEl.querySelectorAll('.session-item').length).toBe(1);

    // Finish "renaming"
    renaming = false;
    guardedRender();
    expect(sessionListEl.querySelectorAll('.session-item').length).toBe(2);
  });

  // ── Title lock indicator tests ──

  it('shows lock badge when session.titleLocked is true', () => {
    sessions.set('session-1', { connected: true, displayName: 'Locked', titleLocked: true });
    renderSidebar();

    const badge = sessionListEl.querySelector('.lock-badge');
    expect(badge).toBeTruthy();
    // S5a: lock is a codicon glyph (font ::before), not emoji text
    expect(badge!.classList.contains('codicon-lock')).toBe(true);
  });

  it('does not show lock badge when titleLocked is false', () => {
    sessions.set('session-1', { connected: true, displayName: 'Unlocked', titleLocked: false });
    renderSidebar();

    expect(sessionListEl.querySelector('.lock-badge')).toBeNull();
  });

  it('does not show lock badge when titleLocked is undefined', () => {
    sessions.set('session-1', { connected: true, displayName: 'Default' });
    renderSidebar();

    expect(sessionListEl.querySelector('.lock-badge')).toBeNull();
  });

  // ── getOrder tests ──

  it('renders sessions in getOrder sequence', () => {
    sessions.set('a', { connected: true, displayName: 'A' });
    sessions.set('b', { connected: true, displayName: 'B' });
    sessions.set('c', { connected: true, displayName: 'C' });
    const render = createRenderSidebar({
      sessionListEl,
      sessions,
      getActiveSessionName: () => 'a',
      onSwitch,
      onRename,
      onRemove,
      onContextMenu,
      getOrder: () => ['c', 'a', 'b'],
    });
    render();
    const names = [...sessionListEl.querySelectorAll('.name')];
    expect(names.map(n => n.textContent)).toEqual(['C', 'A', 'B']);
  });

  it('getOrder skips names not in the sessions Map', () => {
    sessions.set('a', { connected: true, displayName: 'A' });
    sessions.set('b', { connected: true, displayName: 'B' });
    const render = createRenderSidebar({
      sessionListEl,
      sessions,
      getActiveSessionName: () => 'a',
      onSwitch,
      onRename,
      onRemove,
      onContextMenu,
      getOrder: () => ['c', 'a', 'missing', 'b'],
    });
    render();
    const names = [...sessionListEl.querySelectorAll('.name')];
    expect(names.map(n => n.textContent)).toEqual(['A', 'B']);
  });

  it('uses Map insertion order when getOrder is not provided', () => {
    sessions.set('x', { connected: true, displayName: 'X' });
    sessions.set('y', { connected: true, displayName: 'Y' });
    sessions.set('z', { connected: true, displayName: 'Z' });
    // renderSidebar was created without getOrder in beforeEach
    renderSidebar();
    const names = [...sessionListEl.querySelectorAll('.name')];
    expect(names.map(n => n.textContent)).toEqual(['X', 'Y', 'Z']);
  });
});

// ── SP2 Spaces: persisted-record round-trip (arch §8 self-check) ──────────
// The one new algorithm SP2 adds is the tiles binding-map + lock rebuild. This
// asserts a toJSON→fromJSON round-trip preserves panel→binding mapping, the
// opaque dockview geometry blob, AND lock state (grid + per-tile) exactly. The
// live WebGPU geometry survival is proven separately in the real WKWebView.
describe('normalizeSpaceRecord — space persistence round-trip', () => {
  const record = {
    name: 'Backend',
    createdMs: 1737600000000,
    // dockview.toJSON() geometry blob — opaque to us; split ratios live here.
    layout: {
      grid: {
        root: {
          type: 'branch',
          data: [
            { type: 'leaf', data: { views: ['panel_primary'], id: 'g1' }, size: 640 },
            { type: 'leaf', data: { views: ['panel_2'], id: 'g2' }, size: 360 },
          ],
          size: 900,
        },
        width: 1000, height: 900, orientation: 'HORIZONTAL',
      },
      panels: { panel_primary: { id: 'panel_primary' }, panel_2: { id: 'panel_2' } },
    },
    tiles: {
      panel_primary: { kind: 'primary', locked: false },
      panel_2: { kind: 'session', sessionName: 'immorterm-ai-7f3a', locked: true },
    },
    gridLocked: true,
  };

  it('is idempotent', () => {
    expect(normalizeSpaceRecord(normalizeSpaceRecord(record)))
      .toEqual(normalizeSpaceRecord(record));
  });

  it('round-trips bindings + geometry + lock state through JSON exactly', () => {
    const canonical = normalizeSpaceRecord(record);
    // Persist → reload (what save-space + spaces-load do across the wire/disk).
    const reloaded = normalizeSpaceRecord(JSON.parse(JSON.stringify(canonical)));
    expect(reloaded).toEqual(canonical);
    // Geometry blob survives byte-for-byte (split ratios 640/360 preserved).
    expect(reloaded.layout).toEqual(record.layout);
    expect(reloaded.layout.grid.root.data[0].size).toBe(640);
    // Bindings: panelId → kind/sessionName preserved.
    expect(reloaded.tiles.panel_2.sessionName).toBe('immorterm-ai-7f3a');
    expect(reloaded.tiles.panel_primary.kind).toBe('primary');
    // Lock state: whole-grid + per-tile preserved.
    expect(reloaded.gridLocked).toBe(true);
    expect(reloaded.tiles.panel_2.locked).toBe(true);
    expect(reloaded.tiles.panel_primary.locked).toBe(false);
  });

  it('coerces a partial/hand-edited record to the canonical shape', () => {
    const n = normalizeSpaceRecord({ tiles: { p: {} } });
    expect(n.name).toBe('Space');
    expect(n.layout).toBe(null);
    expect(n.gridLocked).toBe(false);
    expect(n.tiles.p).toEqual({ kind: 'session', sessionName: undefined, locked: false });
  });

  it('tolerates null/garbage input without throwing', () => {
    expect(() => normalizeSpaceRecord(null)).not.toThrow();
    expect(normalizeSpaceRecord(null).tiles).toEqual({});
    expect(normalizeSpaceRecord({ tiles: 'nope' }).tiles).toEqual({});
  });
});
