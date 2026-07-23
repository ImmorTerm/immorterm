# ImmorTerm — Launch Kit v1

**Voice laws (from brand):** mom-clear (say WHAT IT IS plainly), fun-first, no staccato
fragments, metaphors decorate — never carry the meaning. Mort the axolotl is the face.
**One-liner:** *Your terminal sessions survive crashes and restarts — and your AI remembers
every session.*

---

## 1. Show HN

**Title:** Show HN: ImmorTerm — terminal sessions that survive VS Code crashes, with an AI memory

**Body:**
ImmorTerm keeps your terminal sessions alive when VS Code crashes, restarts, or you close the
window. When you come back, every session is exactly where you left it — same scrollback, same
running processes — and it auto-reconnects. No more losing a long-running job or a carefully
set-up shell because the editor died.

It also gives your AI coding sessions a persistent memory. Every decision, bug root-cause, and
code change is captured locally (your own SQLite) and searchable across sessions, so the next
time you (or Claude) start work, the context is already there — "why did we do it this way?"
has an answer.

- One command: `npx immorterm`
- Runs as a VS Code extension (and a standalone GPU terminal).
- Open storage, open capture — your data stays in your SQLite. The ranking engine is the only
  closed part.
- Mascot: Mort, an axolotl (axolotls regrow what they lose — like your sessions).

We built it because we kept losing terminal state and re-explaining context to our AI. Would
love feedback on the persistence model and the memory search.

Repo: https://github.com/ImmorTerm/immorterm · Site: https://immorterm.com

---

## 2. Announcement thread (X / LinkedIn)

**1/** Your terminal shouldn't die when your editor does.
ImmorTerm keeps your terminal sessions alive across VS Code crashes and restarts — same
scrollback, same running processes, auto-reconnected. Meet Mort. 🦎

**2/** The problem: you kick off a long build, VS Code crashes, and it's gone. Or you restart
and every terminal is a blank slate. ImmorTerm makes sessions persistent — they outlive the
window.

**3/** Then we went further. Your AI coding sessions get a memory. Every decision and code
change is captured — locally, in your own SQLite — and searchable. Start a new session and the
context ("why is it built this way?") is already loaded.

**4/** Open capture, open storage, your SQLite. The only closed piece is the ranking engine.
Your data is yours.

**5/** One command to try it: `npx immorterm`
Site: immorterm.com · It's open source (FSL).

**6/** Mort's an axolotl — the animal that regrows whatever it loses. Felt right for a terminal
that never really dies. Come break it and tell us what survives.

---

## 3. 60-second demo script (for the recorder)

0:00 — Terminal open in VS Code, a long process running (e.g. a build streaming output).
0:06 — Force-quit VS Code entirely. (Beat. The oh-no moment.)
0:10 — Reopen VS Code. The ImmorTerm terminal reconnects — same scrollback, process still
       running. Caption: "It never left."
0:22 — Type a question to Claude: "why did we pick this database schema?" — memory surfaces the
       past decision instantly. Caption: "Your AI remembers."
0:38 — Show the one-command install: `npx immorterm`. Caption: "One command."
0:48 — Mort waves. Wordmark: ImmorTerm. Tag: "Your terminal is immortal now."
0:60 — end.

---

## 4. Founder to-do before posting
- [ ] Extension live on Open VSX / Marketplace (so "install" links work).
- [ ] Record the 60s demo (script above).
- [ ] Confirm claims match shipped reality (memory search UX, auto-reconnect).
- [ ] Post order: Show HN (weekday morning PT) → thread → reply with demo video.
