# ImmorTerm Resource Management

## Source of Truth

**`apps/extension/resources/`** is the **ONLY source of truth** for all scripts and templates.

This folder contains:
- Shell scripts deployed to each project's `.vscode/terminals/`
- Configuration templates (screenrc, zshrc)
- Claude session tracking scripts

## How Updates Work

When the extension version changes, all resources are **automatically re-deployed** to projects.
This is managed by `resource-extractor.ts` which checks `.immorterm-version` in each project.

## Making Changes

1. Edit files in `apps/extension/resources/`
2. Bump version in `package.json`
3. Build and deploy the extension
4. Resources will auto-update on next VS Code reload
