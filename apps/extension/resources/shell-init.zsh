# ImmorTerm Shell Initialization
# This file is sourced automatically for each ImmorTerm screen session
# Provides dynamic window title with "last activity" timestamp
# OPTIMIZED: Uses zsh built-ins to avoid forks

# Only run inside screen sessions
[[ -z "$STY" ]] && return

# Load zsh datetime module for $EPOCHSECONDS (avoids forking to date)
zmodload -F zsh/datetime b:EPOCHSECONDS 2>/dev/null

# Use the same screen binary that created this session (screen vs immorterm use different sockets)
SCREEN_CMD="${IMMORTERM_SCREEN_BINARY:-immorterm}"

# Initialize base name (separate from display name to prevent timestamp pollution)
# IMMORTERM_BASE_NAME stores ONLY the base name, never timestamps
if [[ -z "${IMMORTERM_BASE_NAME:-}" ]]; then
    export IMMORTERM_BASE_NAME="${SCREEN_WINDOW_NAME:-zsh}"
fi

# Track last update time for debouncing (prevents rapid-fire updates during Claude sessions)
_IMMORTERM_LAST_UPDATE=0

# Title lock flag: when 0, the precmd hook syncs from the renames file (allowing Claude/OSC
# to update the title). When 1, syncing is disabled (user's custom name is preserved).
# Read from IMMORTERM_TITLE_LOCKED env var set by the VS Code extension.
# Fallback to name-based heuristic for backward compatibility.
if [[ -n "${IMMORTERM_TITLE_LOCKED:-}" ]]; then
    _IMMORTERM_TITLE_LOCKED="$IMMORTERM_TITLE_LOCKED"
elif [[ "$IMMORTERM_BASE_NAME" =~ ^immorterm-[0-9]+$ ]]; then
    _IMMORTERM_TITLE_LOCKED=0
else
    _IMMORTERM_TITLE_LOCKED=1
fi

# Propagate lock state to screen session env (for %L status bar indicator)
"$SCREEN_CMD" -X setenv IMMORTERM_TITLE_LOCKED "$_IMMORTERM_TITLE_LOCKED" 2>/dev/null

# WORKING VERSION - with debouncing, screen setenv for renames, and title file sync
_immorterm_title_update() {
    # Debounce: skip if updated within last 2 seconds
    # This prevents visual artifacts during rapid terminal output (e.g., Claude interactive)
    # Uses zsh built-in EPOCHSECONDS (no fork) with fallback
    local now=${EPOCHSECONDS:-$(date +%s)}
    if (( now - _IMMORTERM_LAST_UPDATE < 2 )); then
        return
    fi
    _IMMORTERM_LAST_UPDATE=$now

    # Check for pending rename via screen environment (set by VS Code rename command)
    # This replaces the file-based IPC approach for cleaner communication
    local pending_name
    pending_name=$("$SCREEN_CMD" -Q echo '$IMMORTERM_PENDING_RENAME' 2>/dev/null)
    # screen -Q returns the literal string if not set, so check for both empty and unexpanded
    if [[ -n "$pending_name" && "$pending_name" != '$IMMORTERM_PENDING_RENAME' ]]; then
        export IMMORTERM_BASE_NAME="$pending_name"
        _IMMORTERM_TITLE_LOCKED=1  # User renamed — stop syncing from renames file
        # Note: Extension already set IMMORTERM_TITLE_LOCKED=1 in screen env
        # Clear the pending rename so it's not picked up again
        "$SCREEN_CMD" -X setenv IMMORTERM_PENDING_RENAME "" 2>/dev/null
    fi

    # Sync IMMORTERM_BASE_NAME from the title file written by ImmorTerm's C code.
    # When Claude sends OSC 0/2 to change the title, the C code writes it to this file.
    # Without this sync, precmd would overwrite Claude's dynamic title with the stale
    # IMMORTERM_BASE_NAME (e.g., "immorterm-1") every time the prompt renders.
    # Skipped when locked (user has a custom name that shouldn't be overridden).
    if (( ! _IMMORTERM_TITLE_LOCKED )) && [[ -n "${IMMORTERM_RENAMES_DIR:-}" && -n "${STY:-}" ]]; then
        local _session_name="${STY#*.}"  # Strip PID prefix: "51227.immorterm-abc" → "immorterm-abc"
        local _title_file="${IMMORTERM_RENAMES_DIR}/${_session_name}"
        if [[ -f "$_title_file" ]]; then
            local _file_title
            _file_title=$(<"$_title_file")
            if [[ -n "$_file_title" ]]; then
                export IMMORTERM_BASE_NAME="$_file_title"
            fi
        fi
    fi

    # Screen title is just the base name (timestamp shown separately on right side of status bar)
    # Note: Last activity time is tracked via log file mtime, not shell hooks
    "$SCREEN_CMD" -X title "$IMMORTERM_BASE_NAME" 2>/dev/null

    # VS Code tab gets clean name without timestamp
    # OSC 0 sequence goes directly to VS Code's PTY, bypassing screen
    printf '\033]0;%s\007' "$IMMORTERM_BASE_NAME" > /dev/tty
}

# Remove any conflicting screen title hooks from user's .zshrc
# (These might have been registered before shell-init.zsh and would override our clean titles)
precmd_functions=(${precmd_functions:#_screen_title_update})
preexec_functions=(${preexec_functions:#_screen_title_update})

# Register precmd hook
if [[ ! " ${precmd_functions[*]} " =~ " _immorterm_title_update " ]]; then
    precmd_functions+=(_immorterm_title_update)
fi

# Set initial title (deferred to not block shell startup)
# The title will be set on first prompt via precmd hook
# For immediate title, we use printf which doesn't fork
printf '\033]0;%s\007' "$IMMORTERM_BASE_NAME" > /dev/tty 2>/dev/null

# OSC 133 Semantic Prompt Markers
# These markers let the terminal emulator (ImmorTerm AI, VS Code) know
# where prompts, user input, and command output begin and end.
#
# Markers:  A = prompt start,  B = input start (after prompt text),
#           C = command output start,  D = command done (with exit code)
#
# Safe to emit inside screen sessions. Screen silently drops unknown
# OSC types. For ImmorTerm AI (Rust daemon), these flow directly to
# the terminal emulator which tracks them in PromptState.

# Guard: only emit if we haven't already (another integration may have set these)
if [[ "$PROMPT" != *'133;B'* ]]; then

    # D (command done) + A (prompt start). Must be FIRST in precmd to capture $?
    # before any other precmd function can clobber it.
    #
    # OSC 7 (current working directory) is emitted in the same hook so the
    # terminal emulator's `Terminal::cwd` field stays in sync with the
    # shell. Format: `\e]7;file://host/path\e\\`. The standalone Tauri app
    # reads this via the WASM cwd getter to drive the plain→project
    # upgrade banner when the user cd's into a trusted project.
    _immorterm_osc133_precmd() {
        local exit_code=$?
        printf '\e]133;D;%d\e\\' "$exit_code"
        printf '\e]133;A\e\\'
        printf '\e]7;file://%s%s\e\\' "${HOST:-${HOSTNAME:-localhost}}" "$PWD"
    }

    # C (command output start). Fires when user presses Enter.
    _immorterm_osc133_preexec() {
        printf '\e]133;C\e\\'
    }

    # B (input start). Appended to PROMPT so it fires after the prompt text is drawn.
    PROMPT="${PROMPT}"$'%{\e]133;B\e\\%}'

    # Insert our precmd FIRST so $? is captured before _immorterm_title_update
    # (which runs external commands that would overwrite $?)
    precmd_functions=(${precmd_functions:#_immorterm_osc133_precmd})
    precmd_functions=(_immorterm_osc133_precmd "${precmd_functions[@]}")

    # preexec order does not matter. Just register.
    preexec_functions=(${preexec_functions:#_immorterm_osc133_preexec})
    preexec_functions+=(_immorterm_osc133_preexec)
fi

# Helper function to rename window manually
sname() {
    if [[ -z "$1" ]]; then
        echo "Usage: sname <name>           - Rename and lock title"
        echo "       sname --unlock         - Unlock title (allow Claude/OSC changes)"
        echo "       sname !<name>          - Pin title (remove precmd hook)"
        return 1
    fi

    local name="$1"

    if [[ "$name" == "--unlock" ]]; then
        # Unlock mode - allow Claude/OSC to change the title again
        _IMMORTERM_TITLE_LOCKED=0
        "$SCREEN_CMD" -X setenv IMMORTERM_TITLE_LOCKED 0 2>/dev/null
        # Signal VS Code extension to clear the lock via file-based IPC
        # (avoids screen -Q echo which flashes on hardstatus)
        if [[ -n "${IMMORTERM_RENAMES_DIR:-}" && -n "${STY:-}" ]]; then
            local _session_name="${STY#*.}"
            echo '__UNLOCK__' > "${IMMORTERM_RENAMES_DIR}/${_session_name}"
        fi
        echo "Title unlocked — Claude/OSC can change the title"
        # Re-add hook if removed
        if [[ ! " ${precmd_functions[*]} " =~ " _immorterm_title_update " ]]; then
            precmd_functions+=(_immorterm_title_update)
        fi
    elif [[ "$name" == \!* ]]; then
        # Pinned mode - remove hook and set title without date
        precmd_functions=(${precmd_functions:#_immorterm_title_update})
        local pinned_title="${name#!}"
        "$SCREEN_CMD" -X title "$pinned_title" 2>/dev/null
        printf '\033]0;%s\007' "$pinned_title" > /dev/tty
    else
        # Normal mode - update the base name and lock
        export IMMORTERM_BASE_NAME="$name"
        _IMMORTERM_TITLE_LOCKED=1
        "$SCREEN_CMD" -X setenv IMMORTERM_TITLE_LOCKED 1 2>/dev/null
        # Re-add hook if removed
        if [[ ! " ${precmd_functions[*]} " =~ " _immorterm_title_update " ]]; then
            precmd_functions+=(_immorterm_title_update)
        fi
        _immorterm_title_update
    fi
}

# ── Claude Code channel wrapper ──────────────────────────────────────
# PARKED: Claude Code --channels requires --dangerously-load-development-channels
# which shows an interactive confirmation prompt that can't be auto-accepted.
# Uncomment when ImmorTerm becomes an official channel plugin or Claude Code
# adds a way to trust development channels without the prompt.
#
# if [[ "${IMMORTERM_CHANNELS_ENABLED:-true}" != "false" ]] && [[ -n "${IMMORTERM_WINDOW_ID:-}" ]]; then
#     claude() {
#         local real_claude
#         real_claude=$(whence -p claude)  # whence -p skips functions; command -v does NOT in zsh
#         [[ -z "$real_claude" ]] && { echo "claude: command not found" >&2; return 127; }
#         local channel_server="$HOME/.immorterm/bin/immorterm-channel"
#         if [[ -f "$channel_server" ]]; then
#             IMMORTERM_ID="$IMMORTERM_WINDOW_ID" "$real_claude" --dangerously-load-development-channels "server:$channel_server" "$@"
#         else
#             "$real_claude" "$@"
#         fi
#     }
# fi
