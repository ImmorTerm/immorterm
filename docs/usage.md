# Usage Guide

## Daily Workflow

### Opening Terminals

1. **New Terminal**: Press `Ctrl+Shift+`` (backtick) or use `Terminal > New Terminal`
2. Each terminal automatically gets a unique screen session
3. Terminal names are preserved across VS Code restarts

### Terminal Persistence

**When VS Code Crashes or Closes:**
- All running commands continue in background
- When you reopen VS Code, terminals restore automatically
- Scroll history is preserved

**When Your Computer Restarts:**
- Screen sessions are lost (limitation of screen)
- Terminal configurations are preserved
- You can quickly recreate your workspace

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| `Ctrl+Shift+`` | New terminal (with screen session) |
| `Ctrl+Shift+W` | Close terminal and forget session |
| Screen: `Ctrl+A D` | Detach from screen (keeps running) |
| Screen: `Ctrl+A K` | Kill current screen window |
| Screen: `Ctrl+A ?` | Show screen help |

## Managing Terminals

### Renaming Terminals

1. Right-click terminal tab
2. Select "Rename"
3. Name persists across restarts

### Cleanup Commands

**Remove current terminal:**
```bash
.vscode/terminals/screen-forget
```

**Remove stale sessions:**
```bash
.vscode/terminals/screen-cleanup
```

**Reset everything:**
```bash
.vscode/terminals/screen-forget-all
```

## Viewing Logs

Terminal output is logged to `.vscode/terminals/logs/`:

```bash
# View log for a specific session
cat .vscode/terminals/logs/myproject-12345-abc.log

# Tail all logs
tail -f .vscode/terminals/logs/*.log
```

## Troubleshooting

### Terminal not restoring

1. Check if Restore Terminals extension is installed
2. Verify `.vscode/restore-terminals.json` exists
3. Run cleanup: `.vscode/terminals/screen-cleanup`

### Screen session already attached

If you see "There is a screen on... Attached", it means the session is open elsewhere:

```bash
# Force reattach
screen -d -r sessionname
```

### Commands not persisting

Make sure you're using the screen terminal profile:
1. Click dropdown arrow next to `+` in terminal
2. Select the profile with "(Screen)" in the name

## Tips & Tricks

### Long-running processes

Screen is perfect for long-running processes:
```bash
# Start a build and detach
npm run build &
# Ctrl+Shift+W to close VS Code terminal
# Build continues in background!
```

### Multiple projects

Each project gets isolated screen sessions:
- Project names are lowercase
- Sessions named: `{project}-{unique-id}`
- No conflicts between projects

### SSH sessions

Screen works great for SSH:
```bash
ssh user@server
# If connection drops, just reconnect
# Screen session keeps everything running
```
