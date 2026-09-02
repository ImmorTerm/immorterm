import { describe, expect, it } from 'vitest';

import { isNativeOptionText } from '../../resources/gpu-terminal-input.js';

describe('native macOS Option text', () => {
  it('recognizes composed en dash, em dash, and Hebrew maqaf as text', () => {
    expect(isNativeOptionText({ key: '–', altKey: true, shiftKey: false })).toBe(true);
    expect(isNativeOptionText({ key: '—', altKey: true, shiftKey: true })).toBe(true);
    expect(isNativeOptionText({ key: '־', altKey: true, shiftKey: false })).toBe(true);
  });

  it('preserves ASCII Option chords as shell Meta shortcuts', () => {
    expect(isNativeOptionText({ key: 'b', altKey: true })).toBe(false);
    expect(isNativeOptionText({ key: 'Backspace', altKey: true })).toBe(false);
    expect(isNativeOptionText({ key: '-', altKey: false })).toBe(false);
  });

  it('does not intercept Ctrl+Option or Cmd+Option chords', () => {
    expect(isNativeOptionText({ key: '–', altKey: true, ctrlKey: true })).toBe(false);
    expect(isNativeOptionText({ key: '–', altKey: true, metaKey: true })).toBe(false);
  });
});
