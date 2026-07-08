// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { createModalSystem } from '../../resources/gpu-terminal-modals.js';

// ── Test Helpers ─────────────────────────────────────────────────

/** Create mock DOM elements that mimic the modal structure. */
function createMockDom() {
  const modalBackdrop = document.createElement('div');
  modalBackdrop.id = 'modal-backdrop';

  const modalContainer = document.createElement('div');
  modalContainer.id = 'modal-container';

  const modalTitle = document.createElement('div');
  modalTitle.id = 'modal-title';

  const modalBody = document.createElement('div');
  modalBody.id = 'modal-body';

  const modalFooter = document.createElement('div');
  modalFooter.id = 'modal-footer';

  const modalCloseBtn = document.createElement('button');
  modalCloseBtn.id = 'modal-close';

  const canvas = document.createElement('canvas');
  canvas.id = 'terminal-canvas';
  canvas.focus = vi.fn();

  return { modalBackdrop, modalContainer, modalTitle, modalBody, modalFooter, modalCloseBtn, canvas };
}

/** Default prefs object for tests. */
function defaultPrefs() {
  return {
    borderEnabled: true,
    borderOpacity: 1.0,
    statusBarMode: 'always' as const,
    animations: true,
    expressionEffects: true,
    celebrations: true,
    dangerEffects: true,
    textAnimations: true,
  };
}

/** Create a modal system with mocked dependencies. */
function createTestModals(overrides: Record<string, unknown> = {}) {
  const dom = createMockDom();
  const postMessage = vi.fn();
  const dismissPopup = vi.fn();
  const prefs = defaultPrefs();
  const setPrefs = vi.fn((p: Record<string, unknown>) => Object.assign(prefs, p));
  const terminal = null;

  const modals = createModalSystem({
    ...dom,
    postMessage,
    dismissPopup,
    getPrefs: () => ({ ...prefs }),
    setPrefs,
    getTerminal: () => terminal,
    ...overrides,
  });

  return { ...dom, postMessage, dismissPopup, prefs, setPrefs, modals };
}

// ── Core Show/Dismiss ────────────────────────────────────────────

describe('Modal System — Show/Dismiss', () => {
  it('showModal adds visible class to backdrop and container', () => {
    const { modals, modalBackdrop, modalContainer } = createTestModals();
    modals.showModal('diagnostics');
    expect(modalBackdrop.classList.contains('visible')).toBe(true);
    expect(modalContainer.classList.contains('visible')).toBe(true);
  });

  it('showModal sets the modal title', () => {
    const { modals, modalTitle } = createTestModals();
    modals.showModal('diagnostics');
    expect(modalTitle.textContent).toBe('Diagnostics');
  });

  it('showModal sets correct titles for each modal type', () => {
    const expected: Record<string, string> = {
      diagnostics: 'Diagnostics',
      services: 'Services',
      license: 'License',
      logs: 'Session Logs',
      wizard: 'Setup Wizard',
      appearance: 'Personalize',
    };
    for (const [kind, title] of Object.entries(expected)) {
      const { modals, modalTitle } = createTestModals();
      modals.showModal(kind);
      expect(modalTitle.textContent).toBe(title);
    }
  });

  it('showModal clears previous modal body content', () => {
    const { modals, modalBody } = createTestModals();
    modalBody.textContent = 'old content';
    modals.showModal('diagnostics');
    // Body should contain spinner, not old content
    expect(modalBody.textContent).not.toContain('old content');
  });

  it('showModal calls dismissPopup', () => {
    const { modals, dismissPopup } = createTestModals();
    modals.showModal('diagnostics');
    expect(dismissPopup).toHaveBeenCalled();
  });

  it('dismissModal removes visible class', () => {
    const { modals, modalBackdrop, modalContainer } = createTestModals();
    modals.showModal('diagnostics');
    modals.dismissModal();
    expect(modalBackdrop.classList.contains('visible')).toBe(false);
    expect(modalContainer.classList.contains('visible')).toBe(false);
  });

  it('dismissModal refocuses canvas', () => {
    const { modals, canvas } = createTestModals();
    modals.showModal('diagnostics');
    modals.dismissModal();
    expect(canvas.focus).toHaveBeenCalled();
  });

  it('isActive returns false when no modal is shown', () => {
    const { modals } = createTestModals();
    expect(modals.isActive()).toBe(false);
  });

  it('isActive returns true when a modal is shown', () => {
    const { modals } = createTestModals();
    modals.showModal('diagnostics');
    expect(modals.isActive()).toBe(true);
  });

  it('isActive returns false after dismissing', () => {
    const { modals } = createTestModals();
    modals.showModal('diagnostics');
    modals.dismissModal();
    expect(modals.isActive()).toBe(false);
  });

  it('close button click dismisses modal', () => {
    const { modals, modalCloseBtn, modalBackdrop } = createTestModals();
    modals.showModal('diagnostics');
    modalCloseBtn.click();
    expect(modalBackdrop.classList.contains('visible')).toBe(false);
  });

  it('backdrop click dismisses modal', () => {
    const { modals, modalBackdrop } = createTestModals();
    modals.showModal('diagnostics');
    modalBackdrop.click();
    expect(modalBackdrop.classList.contains('visible')).toBe(false);
  });

  it('Escape key dismisses modal', () => {
    const { modals, modalBackdrop } = createTestModals();
    modals.showModal('diagnostics');
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    expect(modalBackdrop.classList.contains('visible')).toBe(false);
  });

  it('Escape key does nothing when no modal is active', () => {
    const { modals, canvas } = createTestModals();
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    // Should not throw and should not call focus (no modal was open)
    expect(modals.isActive()).toBe(false);
  });
});

// ── Appearance Modal ─────────────────────────────────────────────

describe('Modal System — Appearance', () => {
  it('renderAppearanceModal sends get-preferences message', () => {
    const { modals, postMessage } = createTestModals();
    modals.showModal('appearance');
    expect(postMessage).toHaveBeenCalledWith({ type: 'get-preferences' });
  });

  it('renders toggle rows for all preference keys', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('appearance');
    const labels = Array.from(modalBody.querySelectorAll('.modal-row-label'))
      .map(el => el.textContent);
    expect(labels).toContain('Border');
    expect(labels).toContain('Border Opacity');
    expect(labels).toContain('Status Bar');
    expect(labels).toContain('Animations');
    expect(labels).toContain('Expression Effects');
    expect(labels).toContain('Celebrations');
    expect(labels).toContain('Danger Effects');
    expect(labels).toContain('Text Animations');
  });

  it('toggle click calls setPrefs with correct key', () => {
    const { modals, modalBody, setPrefs } = createTestModals();
    modals.showModal('appearance');
    // Find the "Animations" toggle row and click it
    const rows = Array.from(modalBody.querySelectorAll('.modal-row'));
    const animRow = rows.find(r => r.querySelector('.modal-row-label')?.textContent === 'Animations');
    expect(animRow).toBeTruthy();
    animRow!.click();
    expect(setPrefs).toHaveBeenCalledWith({ animations: false });
  });

  it('border toggle sends save-preference message', () => {
    const { modals, modalBody, postMessage } = createTestModals();
    modals.showModal('appearance');
    postMessage.mockClear();
    const rows = Array.from(modalBody.querySelectorAll('.modal-row'));
    const borderRow = rows.find(r => r.querySelector('.modal-row-label')?.textContent === 'Border');
    borderRow!.click();
    expect(postMessage).toHaveBeenCalledWith(
      expect.objectContaining({ type: 'save-preference', key: 'borderEnabled' }),
    );
  });

  it('opacity slider updates value on input', () => {
    const { modals, modalBody, setPrefs } = createTestModals();
    modals.showModal('appearance');
    const slider = modalBody.querySelector('input[type="range"]') as HTMLInputElement;
    expect(slider).toBeTruthy();
    slider.value = '50';
    slider.dispatchEvent(new Event('input'));
    expect(setPrefs).toHaveBeenCalledWith({ borderOpacity: 0.5 });
  });

  it('opacity slider sends save-preference on change', () => {
    const { modals, modalBody, postMessage } = createTestModals();
    modals.showModal('appearance');
    postMessage.mockClear();
    const slider = modalBody.querySelector('input[type="range"]') as HTMLInputElement;
    slider.value = '75';
    slider.dispatchEvent(new Event('input'));
    slider.dispatchEvent(new Event('change'));
    expect(postMessage).toHaveBeenCalledWith(
      expect.objectContaining({ type: 'save-preference', key: 'borderOpacity' }),
    );
  });

  it('status bar segmented control sets mode', () => {
    const { modals, modalBody, setPrefs } = createTestModals();
    modals.showModal('appearance');
    const firstSeg = modalBody.querySelector('.modal-segmented')!;
    const segButtons = firstSeg.querySelectorAll('button');
    expect(segButtons.length).toBe(3);
    // Click 'Hidden' (third button)
    (segButtons[2] as HTMLButtonElement).click();
    expect(setPrefs).toHaveBeenCalledWith({ statusBarMode: 'hidden' });
  });

  it('status bar segmented control highlights active mode', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('appearance');
    const firstSeg = modalBody.querySelector('.modal-segmented')!;
    const segButtons = firstSeg.querySelectorAll('button');
    // 'always' should be active by default
    expect(segButtons[0].classList.contains('active')).toBe(true);
    expect(segButtons[1].classList.contains('active')).toBe(false);
  });

  it('opacity row is hidden when border is disabled', () => {
    const prefs = defaultPrefs();
    prefs.borderEnabled = false;
    const { modals, modalBody } = createTestModals({
      getPrefs: () => ({ ...prefs }),
    });
    modals.showModal('appearance');
    const rows = Array.from(modalBody.querySelectorAll('.modal-row'));
    const opacityRow = rows.find(r => r.querySelector('.modal-row-label')?.textContent === 'Border Opacity');
    expect(opacityRow).toBeTruthy();
    expect(opacityRow!.style.display).toBe('none');
  });
});

// ── Diagnostics Modal ────────────────────────────────────────────

describe('Modal System — Diagnostics', () => {
  it('shows spinner when opened', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('diagnostics');
    expect(modalBody.querySelector('.modal-spinner')).toBeTruthy();
    expect(modalBody.textContent).toContain('Running diagnostics...');
  });

  it('sends run-diagnostics message', () => {
    const { modals, postMessage } = createTestModals();
    modals.showModal('diagnostics');
    expect(postMessage).toHaveBeenCalledWith({ type: 'run-diagnostics' });
  });

  it('handleDiagnosticsResult replaces spinner with results', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('diagnostics');
    modals.handleDiagnosticsResult([
      { name: 'Binary', status: 'pass', detail: 'v2.0' },
      { name: 'Docker', status: 'fail', detail: 'Not found' },
    ]);
    expect(modalBody.querySelector('.modal-spinner')).toBeNull();
    const labels = Array.from(modalBody.querySelectorAll('.modal-row-label'))
      .map(el => el.textContent);
    expect(labels).toContain('Binary');
    expect(labels).toContain('Docker');
  });

  it('handleDiagnosticsResult shows summary in footer', () => {
    const { modals, modalFooter } = createTestModals();
    modals.showModal('diagnostics');
    modals.handleDiagnosticsResult([
      { name: 'A', status: 'pass' },
      { name: 'B', status: 'warn' },
      { name: 'C', status: 'fail' },
    ]);
    expect(modalFooter.style.display).toBe('block');
    expect(modalFooter.textContent).toContain('1 passed');
    expect(modalFooter.textContent).toContain('1 warning');
    expect(modalFooter.textContent).toContain('1 failed');
  });

  it('footer color is red when failures exist', () => {
    const { modals, modalFooter } = createTestModals();
    modals.showModal('diagnostics');
    modals.handleDiagnosticsResult([{ name: 'A', status: 'fail' }]);
    const summary = modalFooter.querySelector('span');
    // jsdom normalizes hex to rgb()
    expect(summary!.style.color).toBe('rgb(243, 139, 168)');
  });

  it('footer color is green when all pass', () => {
    const { modals, modalFooter } = createTestModals();
    modals.showModal('diagnostics');
    modals.handleDiagnosticsResult([{ name: 'A', status: 'pass' }]);
    const summary = modalFooter.querySelector('span');
    expect(summary!.style.color).toBe('rgb(166, 227, 161)');
  });
});

// ── Services Modal ───────────────────────────────────────────────

describe('Modal System — Services', () => {
  it('shows spinner when opened', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('services');
    expect(modalBody.querySelector('.modal-spinner')).toBeTruthy();
  });

  it('sends get-service-status message', () => {
    const { modals, postMessage } = createTestModals();
    modals.showModal('services');
    expect(postMessage).toHaveBeenCalledWith({ type: 'get-service-status' });
  });

  it('handleServiceStatus renders service cards', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('services');
    modals.handleServiceStatus([
      { id: 'mem', name: 'Memory', enabled: true, healthy: true, desc: 'AI memory', canStartStop: true },
    ]);
    expect(modalBody.querySelector('.modal-spinner')).toBeNull();
    expect(modalBody.textContent).toContain('Memory');
    expect(modalBody.textContent).toContain('AI memory');
  });

  it('renders start/stop buttons for services with canStartStop', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('services');
    modals.handleServiceStatus([
      { id: 'mem', name: 'Memory', enabled: true, healthy: true, desc: '', canStartStop: true },
    ]);
    const buttons = Array.from(modalBody.querySelectorAll('.modal-btn'))
      .map(b => b.textContent);
    expect(buttons).toContain('Start');
    expect(buttons).toContain('Stop');
  });

  it('start button is disabled when service is healthy', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('services');
    modals.handleServiceStatus([
      { id: 'mem', name: 'Memory', enabled: true, healthy: true, desc: '', canStartStop: true },
    ]);
    const startBtn = Array.from(modalBody.querySelectorAll('.modal-btn'))
      .find(b => b.textContent === 'Start') as HTMLButtonElement;
    expect(startBtn.disabled).toBe(true);
  });

  it('stop button sends service-action message on click', () => {
    const { modals, modalBody, postMessage } = createTestModals();
    modals.showModal('services');
    modals.handleServiceStatus([
      { id: 'mem', name: 'Memory', enabled: true, healthy: true, desc: '', canStartStop: true },
    ]);
    postMessage.mockClear();
    const stopBtn = Array.from(modalBody.querySelectorAll('.modal-btn'))
      .find(b => b.textContent === 'Stop') as HTMLButtonElement;
    stopBtn.click();
    expect(postMessage).toHaveBeenCalledWith({ type: 'service-action', serviceId: 'mem', action: 'stop' });
  });

  it('toggle sends service-toggle message on click', () => {
    const { modals, modalBody, postMessage } = createTestModals();
    modals.showModal('services');
    modals.handleServiceStatus([
      { id: 'mem', name: 'Memory', enabled: true, healthy: true, desc: '', canStartStop: false },
    ]);
    postMessage.mockClear();
    const toggle = modalBody.querySelector('.modal-toggle-wrap');
    toggle!.click();
    expect(postMessage).toHaveBeenCalledWith({ type: 'service-toggle', serviceId: 'mem', enabled: false });
  });
});

// ── License Modal ────────────────────────────────────────────────

describe('Modal System — License', () => {
  it('shows spinner when opened', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('license');
    expect(modalBody.querySelector('.modal-spinner')).toBeTruthy();
  });

  it('sends get-license-status message', () => {
    const { modals, postMessage } = createTestModals();
    modals.showModal('license');
    expect(postMessage).toHaveBeenCalledWith({ type: 'get-license-status' });
  });

  it('handleLicenseStatus renders Pro badge for Pro users', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('license');
    modals.handleLicenseStatus({ isPro: true, email: 'test@example.com', expiresAt: '2027-01-01', key: 'ABCDEF1234567890' });
    expect(modalBody.querySelector('.modal-badge.pro')).toBeTruthy();
    expect(modalBody.textContent).toContain('Pro');
  });

  it('handleLicenseStatus renders Free badge for free users', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('license');
    modals.handleLicenseStatus({ isPro: false });
    expect(modalBody.querySelector('.modal-badge.free')).toBeTruthy();
    expect(modalBody.textContent).toContain('Free');
  });

  it('Pro view shows validate and deactivate buttons', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('license');
    modals.handleLicenseStatus({ isPro: true, email: 'test@example.com', key: 'ABCDEF12' });
    const buttons = Array.from(modalBody.querySelectorAll('.modal-btn'))
      .map(b => b.textContent);
    expect(buttons).toContain('Validate');
    expect(buttons).toContain('Deactivate');
  });

  it('Free view shows license key input and activate button', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('license');
    modals.handleLicenseStatus({ isPro: false });
    expect(modalBody.querySelector('.modal-input')).toBeTruthy();
    const buttons = Array.from(modalBody.querySelectorAll('.modal-btn'))
      .map(b => b.textContent);
    expect(buttons).toContain('Activate');
  });

  it('activate button sends license-activate message', () => {
    const { modals, modalBody, postMessage } = createTestModals();
    modals.showModal('license');
    modals.handleLicenseStatus({ isPro: false });
    postMessage.mockClear();
    const input = modalBody.querySelector('.modal-input') as HTMLInputElement;
    input.value = 'TEST-KEY-123';
    const activateBtn = Array.from(modalBody.querySelectorAll('.modal-btn'))
      .find(b => b.textContent === 'Activate') as HTMLButtonElement;
    activateBtn.click();
    expect(postMessage).toHaveBeenCalledWith({ type: 'license-activate', key: 'TEST-KEY-123' });
  });

  it('handleLicenseActionResult shows error in modal body', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('license');
    modals.handleLicenseActionResult({ error: 'Invalid key' });
    expect(modalBody.textContent).toContain('Invalid key');
  });

  it('handleLicenseActionResult on success requests updated status', () => {
    const { modals, postMessage } = createTestModals();
    modals.showModal('license');
    postMessage.mockClear();
    modals.handleLicenseActionResult({ success: true });
    expect(postMessage).toHaveBeenCalledWith({ type: 'get-license-status' });
  });
});

// ── Logs Modal ───────────────────────────────────────────────────

describe('Modal System — Logs', () => {
  it('shows spinner when opened', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('logs');
    expect(modalBody.querySelector('.modal-spinner')).toBeTruthy();
  });

  it('sends get-session-logs message', () => {
    const { modals, postMessage } = createTestModals();
    modals.showModal('logs');
    expect(postMessage).toHaveBeenCalledWith({ type: 'get-session-logs' });
  });

  it('handleSessionLogs shows empty state when no sessions', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('logs');
    modals.handleSessionLogs([]);
    expect(modalBody.textContent).toContain('No session logs found');
  });

  it('handleSessionLogs renders session rows', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('logs');
    modals.handleSessionLogs([
      { name: 'session-1', alive: true, types: ['grid', 'ai'], age: '2h', size: '1.2M' },
      { name: 'session-2', alive: false, types: ['grid'], age: '5h', size: '500K' },
    ]);
    expect(modalBody.textContent).toContain('session-1');
    expect(modalBody.textContent).toContain('session-2');
  });

  it('session row click opens session log', () => {
    const { modals, modalBody, postMessage } = createTestModals();
    modals.showModal('logs');
    modals.handleSessionLogs([
      { name: 'test-session', alive: true, types: ['grid'], age: '1h', size: '100K' },
    ]);
    postMessage.mockClear();
    const row = modalBody.querySelector('.modal-row') as HTMLElement;
    row.click();
    expect(postMessage).toHaveBeenCalledWith({ type: 'open-session-log', sessionName: 'test-session', logType: 'grid' });
  });
});

// ── Wizard Modal ─────────────────────────────────────────────────

/** Stub fetch so the wizard vendors step can probe + persist in jsdom. */
function stubWizardFetch() {
  const respond = (obj: unknown) =>
    Promise.resolve({
      json: () => Promise.resolve(obj),
      text: () => Promise.resolve(JSON.stringify(obj)),
    });
  vi.stubGlobal('fetch', vi.fn((url: unknown) => {
    const u = String(url);
    if (u.includes('/api/info')) return respond({ projectDir: '/tmp/test-project' });
    if (u.includes('/vendors/detect')) return respond({ vendors: [] });
    return respond({});
  }));
}

/** Let the wizard's pending fetch chains settle. */
async function flushWizard() {
  await new Promise((r) => setTimeout(r, 0));
  await new Promise((r) => setTimeout(r, 0));
}

/** Click through welcome + vendors to land on the binary step. */
async function advanceToBinary(modalFooter: HTMLElement) {
  (modalFooter.querySelector('.modal-btn.primary') as HTMLButtonElement).click(); // welcome → vendors (probe)
  await flushWizard(); // vendor grid renders
  const saveNext = Array.from(modalFooter.querySelectorAll('.modal-btn'))
    .find((b) => b.textContent === 'Save & Next') as HTMLButtonElement;
  saveNext.click(); // persist selection → binary
  await flushWizard();
}

describe('Modal System — Wizard', () => {
  beforeEach(() => {
    stubWizardFetch();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('starts at step 0 (welcome)', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('wizard');
    expect(modalBody.textContent).toContain('Welcome to ImmorTerm!');
  });

  it('shows step dots', () => {
    const { modals, modalBody } = createTestModals();
    modals.showModal('wizard');
    expect(modalBody.querySelectorAll('.modal-step-dot').length).toBe(7);
  });

  it('Get Started button advances to step 1 (vendors)', () => {
    const { modals, modalBody, modalFooter } = createTestModals();
    modals.showModal('wizard');
    // Click "Get Started" in the footer
    const nextBtn = modalFooter.querySelector('.modal-btn.primary') as HTMLButtonElement;
    expect(nextBtn.textContent).toBe('Get Started');
    nextBtn.click();
    // Step 1 is 'vendors' — starts by probing installed AI tools
    expect(modalBody.textContent).toContain('Detecting installed AI tools');
  });

  it('vendors step defaults to Claude Code only', async () => {
    const { modals, modalBody, modalFooter } = createTestModals();
    modals.showModal('wizard');
    (modalFooter.querySelector('.modal-btn.primary') as HTMLButtonElement).click();
    await flushWizard();
    expect(modalBody.textContent).toContain('Which AI tools do you use?');
    // Summary lives in the footer; only Claude Code should be pre-enabled
    expect(modalFooter.textContent).toContain('1 of');
  });

  it('handleWizardCheckResult renders binary check result', async () => {
    const { modals, modalBody, modalFooter } = createTestModals();
    modals.showModal('wizard');
    await advanceToBinary(modalFooter);
    // Receive check result
    modals.handleWizardCheckResult('binary', { found: true, version: 'v2.0', path: '/usr/local/bin/immorterm' });
    expect(modalBody.textContent).toContain('ImmorTerm Binary');
    expect(modalBody.textContent).toContain('v2.0');
  });

  it('wizard step navigation — back button goes to previous step', async () => {
    const { modals, modalBody, modalFooter } = createTestModals();
    modals.showModal('wizard');
    await advanceToBinary(modalFooter);
    // Receive result so nav buttons appear
    modals.handleWizardCheckResult('binary', { found: true });
    // Click back
    const backBtn = Array.from(modalFooter.querySelectorAll('.modal-btn'))
      .find(b => b.textContent === 'Back') as HTMLButtonElement;
    expect(backBtn).toBeTruthy();
    backBtn.click();
    // Should be back at the vendors step (already probed → grid renders)
    expect(modalBody.textContent).toContain('Which AI tools do you use?');
  });

  it('handleWizardActionResult re-checks the current step', async () => {
    const { modals, modalFooter, postMessage } = createTestModals();
    modals.showModal('wizard');
    await advanceToBinary(modalFooter);
    postMessage.mockClear();
    modals.handleWizardActionResult();
    expect(postMessage).toHaveBeenCalledWith({ type: 'wizard-check', step: 'binary' });
  });

  it('last step shows Done button that dismisses modal', async () => {
    const { modals, modalBody, modalFooter, modalBackdrop } = createTestModals();
    modals.showModal('wizard');
    // welcome → vendors → binary
    await advanceToBinary(modalFooter);
    // binary → docker → services, providing results so nav appears
    modals.handleWizardCheckResult('binary', { found: true });
    (modalFooter.querySelector('.modal-btn.primary') as HTMLButtonElement).click();
    modals.handleWizardCheckResult('docker', { status: 'running' });
    (modalFooter.querySelector('.modal-btn.primary') as HTMLButtonElement).click();
    modals.handleWizardCheckResult('services', { services: [] });
    // services → license → complete
    (modalFooter.querySelector('.modal-btn.primary') as HTMLButtonElement).click();
    (modalFooter.querySelector('.modal-btn.primary') as HTMLButtonElement).click();
    // Should be on 'complete' step
    expect(modalBody.textContent).toContain('Setup complete!');
    const doneBtn = modalFooter.querySelector('.modal-btn.primary') as HTMLButtonElement;
    expect(doneBtn.textContent).toBe('Done');
    doneBtn.click();
    expect(modalBackdrop.classList.contains('visible')).toBe(false);
  });
});
