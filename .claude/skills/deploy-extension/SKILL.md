---
name: deploy-extension
description: "Compile, lint, and deploy VS Code extension changes (TypeScript + HTML/CSS/JS resources). Runs oxlint + node --check on terminal HTML, bun compile for TS, and deploys to installed extension. Use after changes to apps/extension/ that don't involve WASM or Rust."
allowed-tools: Bash, Read
---

# Deploy VS Code Extension

Compile, lint, and deploy extension changes with full quality gates.

## WHEN TO USE THIS

Use `/deploy-extension` after changing ANY of these:

| What changed | Quality gate |
|-------------|-------------|
| `apps/extension/src/*.ts` | TypeScript compilation (`bun run compile`) |
| `apps/extension/resources/gpu-terminal.html` | Oxlint (no-undef) + `node --check` syntax validation |
| `apps/extension/resources/gpu-terminal.css` | Deployed with resources |
| `apps/extension/resources/gpu-terminal-utils.js` | Oxlint (no-undef) + `node --check` syntax validation |
| `apps/extension/resources/*.html` | Oxlint + syntax check on all terminal HTML |
| `apps/extension/resources/shell-init.zsh` | Deployed with resources |
| `libs/menu-data/` or other `@immorterm/*` libs | Lib rebuild + node_modules sync |

**NEVER manually `cp -r resources/*` or `cp -r out/*` without running this skill first.** Manual copies skip all quality gates.

## Steps

Run these steps IN ORDER. Stop on any failure.

```bash
# ── Step 1: Lint terminal HTML (JS syntax + undefined references) ──
OXLINT_CONFIG="$PWD/.oxlintrc-terminal.json"
FAIL=0
for HTML_FILE in apps/extension/resources/*-terminal.html; do
  [ -f "$HTML_FILE" ] || continue
  BASENAME=$(basename "$HTML_FILE")
  JS_TMP=$(mktemp /tmp/terminal-js-check.XXXXXX.mjs)
  sed -n '/<script type="module">/,/^  <\/script>$/{ /<script/d; /<\/script>/d; p; }' "$HTML_FILE" > "$JS_TMP"
  echo "Checking $BASENAME..."
  if ! node --check "$JS_TMP" 2>&1; then
    echo "  FAIL: Syntax error in $BASENAME"
    FAIL=1
  fi
  if ! npx oxlint -c "$OXLINT_CONFIG" --deny no-undef "$JS_TMP" 2>&1; then
    echo "  FAIL: Undefined reference in $BASENAME"
    FAIL=1
  fi
  rm -f "$JS_TMP"
done
[ "$FAIL" -eq 0 ] && echo "Lint OK" || exit 1

# ── Step 2: Compile TypeScript ──
cd apps/extension && bun run compile
cd -

# ── Step 3: Deploy to installed extension ──
EXT_DIR="$HOME/.vscode/extensions/immorterm.immorterm-terminal-1.0.4"
if [ -d "$EXT_DIR" ]; then
  cp -r apps/extension/out/* "$EXT_DIR/out/"
  cp -r apps/extension/resources/* "$EXT_DIR/resources/"

  # Fix broken @immorterm/* symlinks (bun workspace uses relative symlinks)
  DEST_NM="$EXT_DIR/node_modules/@immorterm"
  if [ -d "$DEST_NM" ]; then
    for pkg in "$DEST_NM"/*/; do
      [ -L "${pkg%/}" ] || continue
      target=$(readlink "${pkg%/}")
      resolved="$DEST_NM/$target"
      if [ ! -d "$resolved" ]; then
        src_pkg="$PWD/apps/extension/node_modules/@immorterm/$(basename "${pkg%/}")"
        real_src=$(cd "$src_pkg" 2>/dev/null && pwd -P)
        if [ -d "$real_src" ]; then
          rm -f "${pkg%/}"
          cp -r "$real_src" "${pkg%/}"
          echo "Fixed broken symlink: @immorterm/$(basename "${pkg%/}")"
        fi
      fi
    done
  fi
  echo "Deployed to $EXT_DIR"
else
  echo "SKIP: Extension not installed at $EXT_DIR"
fi

# ── Step 4: Verify ──
diff -rq apps/extension/out/ "$EXT_DIR/out/" 2>/dev/null | head -5
diff -rq apps/extension/resources/ "$EXT_DIR/resources/" 2>/dev/null | grep -v '.gitignore' | head -5
```

## What It Catches

| Gate | Catches |
|------|---------|
| `node --check` | JavaScript syntax errors in HTML `<script>` blocks |
| Oxlint `--deny no-undef` | Undefined variable/function references (typos, missing imports) |
| `bun run compile` | TypeScript type errors, missing imports, broken references |
| Symlink fix | Broken `@immorterm/*` shared lib references in installed extension |

## After Deploying

Reload VS Code: `Cmd+Shift+P` > "Developer: Reload Window"

## When to Use Other Skills Instead

| Situation | Use |
|-----------|-----|
| Changed Rust crates (core/render/daemon) | `/deploy-immorterm-ai` (WASM) + `/deploy-daemon` (native) |
| Changed WASM or shaders | `/deploy-immorterm-ai` |
| Changed only daemon Rust code | `/deploy-daemon` |
