# Named-profile isolation for the self-driven browser (deferred)

Status: **not built.** Deferred from wave 3b. This note records the approach so
a later pass doesn't rediscover it.

## Why it's deferred

The self-driven browser is **one process per user** (broker / route-to-owner
model — see `browser_lock.rs`). It launches a single headful Chromium with a
persistent `--user-data-dir` so the *user's* logins survive restarts. That
persistent profile is the whole point: the human signs in once and the AI
reuses the session.

`Target.createBrowserContext` creates an **incognito** browser context — a
fresh, isolated cookie/storage jar that does NOT share the persistent profile.
So "named profiles" and "persistent logins" pull in opposite directions:

- A named context is isolated → the user's existing logins are gone in it.
- Sharing the persistent profile → no isolation.

Half-building this (create a context but still pin to the default page target,
or create contexts without a lifecycle to dispose them) would leak contexts and
confuse the ref/screencast/target-follow logic, which all assume one active
page target. So: leave it out until there's a concrete need.

## If it's ever wanted — the shape

1. **Context registry.** Add `contexts: HashMap<String /*name*/, String /*browserContextId*/>`
   to `BrowserSession`. `Target.createBrowserContext` returns a
   `browserContextId`; stash it by name.
2. **Create a page in the context.** `Target.createTarget { url, browserContextId }`
   returns a `targetId`. Attach to it via the existing `attach_target` path so
   refs/screencast/console/network re-bind to the new page.
3. **Switch semantics.** A "use profile X" tool switches the pinned target to
   that context's page (or creates one if none). `tabs_list`/`tabs_switch`
   already operate on page targets — they'd need to filter/annotate by context.
4. **Disposal.** `Target.disposeBrowserContext { browserContextId }` on close of
   a named profile; dispose ALL on `close()` before the process teardown.
5. **Persistence caveat.** Incognito contexts are ephemeral — nothing persists
   across process restarts. If a named profile must persist, that's a *second
   `--user-data-dir`* and therefore a *second browser process*, which breaks the
   one-per-user broker invariant. Decide that tradeoff explicitly before
   building.

## Recommendation

Only build this if a real use case appears (e.g. "log into two accounts of the
same site at once"). For that specific case, ephemeral incognito contexts are
the right tool and the persistence caveat doesn't bite. For anything that needs
durable separate logins, reconsider the one-per-user model first.
