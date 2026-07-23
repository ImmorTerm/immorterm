# Memory Product Map

> Working doc — the "what is our memory business, actually" map.
> One engine, three businesses, a handful of open decisions.
> Edit inline; decisions are marked **DECISION** with a recommendation.

---

## The one thing that unmesses the head

There is **one engine**: `immorterm-memory` (the Rust binary, serves on `:8765`).
Everything else labelled "memory" is just **distribution + who runs it + who pays**.
The "$9 memory" confusion comes from blending three different businesses into one SKU.

---

## Every "memory" surface, decoded

| Surface | What it actually is | Who consumes it | Needed? |
|---|---|---|---|
| `immorterm-memory` (npm binary) | The engine, run locally on `:8765` | ImmorTerm terminal users with memory ON; laor (npm) | ✅ core |
| The hooks | Capture → digest → memory wiring, installed by CLI/extension onboarding when memory is enabled | Same local users (laor has the multi-vendor hooks) | ✅ makes memory automatic |
| `immorterm-memory` (Docker) | The same engine, containerized | Platforms self-hosting on servers (delulus) | ✅ just published to the immorterm org |
| `packs.immorterm.com` | Pack lifecycle API: turn a platform's resources into a compiled knowledge base (`.impack`) — "NotebookLM for agents" | delulus (`{slug}-input` / `{slug}-research`); future platforms | ✅ a real product |
| `memory.immorterm.com` | The engine deployed as a hosted service | **Nobody, externally** — grepped cloud + delulus + laor; no code points at that hostname | ❓ **decide** (see below) |
| `.impack` | Transport format (compiled pack zip). The engine is always the runtime that reads it. | Produced by packs, imported into any memory runtime | ✅ the artifact |

Note on `memory.immorterm.com`: delulus self-hosts its own; laor is npm-local; the ai-worker uses an in-cluster `MEMORY_URL`. So the public hostname has no external consumer today.

---

## The three businesses you're blending

### 1. Memory-as-a-feature (for ImmorTerm users)
Local engine + hooks on a dev's machine.
This is **$9/mo Memory Pro** (and $29 Pro = the terminal). **End users.**

### 2. Mem-Packs — "NotebookLM for agents" (`packs.immorterm.com`)
Platforms build a knowledge base from their **own resources**.
This is delulus's entire model. **B2B, usage/tier-based — NOT a $9 consumer SKU.** Undefined today.

### 3. Self-hosting the engine (npm / Docker)
Platforms embed memory in their product (delulus, laor).
The engine is **already public + free** (npm + public GitHub releases).
So you don't charge for self-hosting the engine — you charge for the **services around it** (pack compilation, or a managed memory cloud).

---

## Decisions to make

### DECISION 1 — What IS `memory.immorterm.com`?
Right now: no external consumer. At most internal digestion infra for packs, exposed on a public URL for no reason.

- **(a) Demote it** — it's internal; drop the public ingress. Simplest.
- **(b) Make it a product** — "Managed Memory Cloud": *"don't self-host the container, point your API/MCP here, we run it."* A real B2B offering for teams who don't want to run Docker — but it needs **auth + multi-tenancy + billing**, none of which exist yet.

**Recommendation:** (a) for launch — demote to internal, no public ingress. Revisit (b) only if a real platform asks for managed hosting. *(Fact still to confirm: what `MEMORY_URL` in the cluster actually points at — settles this with certainty.)*

### DECISION 2 — Engine = free to self-host?
It's already public on npm + releases, so effectively **yes** already.

- Free engine, monetize the **services** (open-core — same model as ringtail) — **recommended**
- vs. a commercial embedding license

**Recommendation:** free engine (open-core). delulus/laor self-hosting is fine and unpaid; they pay for **packs** (the compile service) or **managed memory**.

### DECISION 3 — Three separate price axes (this is the "$9 mess")
| Axis | Who | Pricing | Status |
|---|---|---|---|
| ImmorTerm Pro $29 / Memory Pro $9 | end-user, local | fixed | keep as-is |
| Mem-Packs | platform | per-compile or monthly | **undefined — needs a number** |
| Managed Memory Cloud (only if 1b) | platform | per-seat / usage | undefined |

**Keep these axes separate.** Don't tangle the B2B side with the $9 consumer feature.

### DECISION 4 — The landing page
= the storefront for **#2 and #3** (Mem-Packs + self-host/managed memory).
A **separate product page** from the immorterm terminal site. That's why it felt like "a product, not a doc."

### DECISION 5 — De-lonormaly is a prerequisite
You can't sell a self-host image or a hosted service built by a repo you're about to retire.
That's exactly what the in-progress refactor fixes. **Blocks selling any of the above.**

---

## Recommendation (the whole thing in one breath)

**Open-core.**
- The **engine is free** (self-host via npm/Docker, unpaid).
- Monetize **two hosted services**: **Mem-Packs** (the compile/KB product, delulus's use case) and *optionally* **Managed Memory Cloud** (`memory.immorterm.com` reborn with auth + billing).
- The **$9 Memory Pro** stays purely the terminal feature and shouldn't touch the B2B side at all.

---

## Open threads / next facts to nail

- [ ] Confirm what the cluster `MEMORY_URL` points at → settles DECISION 1 with fact, not inference.
- [ ] De-lonormaly cutover (prerequisite for all selling) — memory container published to `ghcr.io/immorterm/immorterm-memory`; founder to make it pullable + provide cluster access.
- [ ] Number for Mem-Packs pricing (DECISION 3).
- [ ] Landing page for Mem-Packs + self-host (DECISION 4), in `~/Development/laor`.
