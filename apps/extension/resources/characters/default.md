---
id: default
name: Default
description: Vendor's native voice — no character override
emoji: ""
---

<!--
  The default character is intentionally empty. When a session's speakMode
  resolves to "default", the user-prompt hook skips injection entirely so
  the AI responds in its native voice. The one-shot transition nudge (used
  when turning OFF a persona like caveman) lives in the hook itself, not
  in this file — it fires exactly once per transition, not on every prompt.
-->
