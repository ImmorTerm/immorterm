/**
 * SINGLE SOURCE OF TRUTH for the VS Code extension id across the monorepo.
 *
 * The published id is `immorterm.immorterm-terminal` (publisher `immorterm` +
 * apps/extension package.json name `immorterm-terminal`). It is NOT
 * `immorterm.immorterm` because the CLI workspace package is already named
 * `immorterm`.
 *
 * Deliberately kept in its own dependency-free module (no `import.meta`, no
 * Node built-ins) so the VS Code extension — whose tsconfig is
 * `module: commonjs` — can import this value directly without dragging in
 * versions.ts. Every TS consumer imports EXTENSION_ID instead of hardcoding.
 */
export const EXTENSION_ID = "immorterm.immorterm-terminal";
