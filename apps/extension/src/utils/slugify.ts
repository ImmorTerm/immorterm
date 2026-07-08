/**
 * Convert a display name to a URL-safe slug for directory names.
 *
 * "Meaning of Life" → "meaning-of-life"
 * "Claude Code — Session 3" → "claude-code-session-3"
 */
export function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-|-$/g, '')
    .substring(0, 50);
}
