---
name: immorterm-workshop-diagrams
description: Use whenever the answer would benefit from a diagram, flow, architecture map, state machine, comparison, or any visual structure. Renders crisp colored boxes + animated connectors + hover tooltips via HTML+CSS+JS in draw_html overlays. Replaces ASCII art, Mermaid, and most hand-crafted SVG.
---

# ImmorTerm — Diagrams as HTML+CSS+JS

When you need a diagram, **draw it as HTML+CSS+JS in a `draw_html` overlay**. Not ASCII art. Not Mermaid (cramped output). Not SVG unless you need precise geometric shapes.

## Why HTML first

| Approach | Verdict |
|---|---|
| HTML+CSS+JS (boxes, tooltips, animation) | **WIN** — bold colors, big text, interactive, scales |
| Hand-crafted SVG | OK as fallback — works but small fonts, no interactivity by default |
| Mermaid via CDN | **AVOID** — tiny boxes, sparse layout, wastes space, ugly defaults |
| ASCII art (plain text) | **NEVER** — font alignment fragile, no styling, no semantics |

## Canonical template — copy this, edit the nodes

```python
immorterm_draw_html(
  session="<SESSION_ID>",
  anchor="scroll",
  name="my-diagram",   # optional, lets you eval_in_workshop to mutate later
  html='''
<div style="width:720px;height:460px;position:relative;background:#181825;border:1px solid #313244;border-radius:12px;padding:14px;font-family:-apple-system,sans-serif;color:#cdd6f4;box-sizing:border-box">
<div style="text-align:center;font-size:13px;font-weight:700">My Diagram Title</div>
<style>
.n{position:absolute;width:140px;padding:10px;background:#1e1e2e;border:1.5px solid;border-radius:10px;transition:all .18s}
.n:hover{transform:scale(1.05);z-index:5;box-shadow:0 6px 24px rgba(0,0,0,.5);background:#2a2a3e}
.n .t{display:none;position:absolute;top:100%;left:0;right:0;margin-top:6px;padding:8px;background:#11111b;border:1px solid currentColor;border-radius:8px;font-size:10.5px;line-height:1.5;color:#cdd6f4;z-index:6}
.n:hover .t,.n.p .t{display:block}
.n .b{display:inline-block;margin-top:6px;padding:1px 7px;font-size:9px;font-weight:700;background:#313244;border-radius:10px;cursor:pointer}
.n .b:hover{background:#45475a}
.c{position:absolute;height:2px;background:linear-gradient(90deg,transparent,currentColor 30%,currentColor 70%,transparent);opacity:.5}
.c::after{content:"";position:absolute;top:-2px;left:0;width:24px;height:6px;background:currentColor;border-radius:6px;animation:f 2s linear infinite;box-shadow:0 0 8px currentColor}
@keyframes f{0%{left:-12%;opacity:0}15%{opacity:1}85%{opacity:1}100%{left:100%;opacity:0}}
</style>

<!-- NODE template — color picks the accent (border + connector + glow) -->
<div class="n" style="top:64px;left:20px;color:#89b4fa">
  <b>Box Title</b>
  <div style="font-size:10px;color:#a6adc8">subtitle</div>
  <span class="b" data-p>i</span>
  <div class="t">Tooltip text. Shows on hover OR when "i" is clicked (pin/unpin).</div>
</div>

<!-- CONNECTOR — left/top/width position; color drives the glow -->
<div class="c" style="top:95px;left:160px;width:40px;color:#89b4fa"></div>

<script>root.querySelectorAll("[data-p]").forEach(b=>b.addEventListener("click",e=>{e.stopPropagation();b.closest(".n").classList.toggle("p")}))</script>
</div>
'''
)
```

## Layout patterns

### 4-box row (architecture sweep)
```
x positions:  20, 200, 380, 560   (180px stride, 140px wide nodes)
y position:   64                  (vertically centered in 460px panel)
connectors:   160, 340, 520        (between adjacent nodes)
```

### 2-row grid (8 components)
```
row 1:  top:64,  left=[20, 200, 380, 560]
row 2:  top:230, left=[20, 200, 380, 560]
connectors between rows: vertical with transform:rotate(90deg);transform-origin:0 0
```

### Sequence diagram (vertical timeline)
- Each actor is a "lifeline" — 28px-tall label at top + dashed `position:absolute` vertical line at x=label-center, height = panel-height
- Messages are horizontal lines from actor-A's lifeline to actor-B's lifeline at specific y positions
- Use ① ② ③ Unicode circled numbers for step indicators
- Italicized labels (`<i>...</i>`) for "internal" actor actions ("hook fires", "renders panel")

## Catppuccin Mocha palette — pick colors per element

| Color | Hex | Use for |
|---|---|---|
| Base | `#181825` | Background of card/panel |
| Surface0 | `#1e1e2e` | Box body fill |
| Surface1 | `#313244` | Box body alt, badges |
| Surface2 | `#45475a` | Badge hover, borders |
| Text | `#cdd6f4` | Primary text |
| Subtext | `#a6adc8` | Secondary labels |
| Muted | `#7f849c` | Tertiary captions |
| Blue | `#89b4fa` | Extension, infrastructure |
| Lavender | `#b4befe` | Daemons, services |
| Mauve | `#cba6f7` | Webviews, frontends |
| Green | `#a6e3a1` | AI, success states |
| Yellow | `#f9e2af` | Warnings, "optional" components |
| Peach | `#fab387` | Hooks, triggers |
| Red | `#f38ba8` | User, deletions, alerts |
| Teal | `#94e2d5` | Workshops, panels |
| Pink | `#f5c2e7` | Special / highlight |

Color per node — don't make everything the same color. Color signals semantic category.

## Animation cookbook

### Flowing-dash connectors (data flow direction)
The default `.c` class in the template above shows a glowing packet repeatedly traversing each connector left-to-right.

### Pulsing nodes (event sequence)
```css
.pulse{animation:pulse 1.4s ease-in-out infinite}
@keyframes pulse{0%,100%{box-shadow:0 0 0 0 currentColor}50%{box-shadow:0 0 0 8px transparent}}
```

### Stagger delays for sequential animations
Add `animation-delay: 0.4s`, `0.8s`, `1.2s` on subsequent elements to show ORDER of events.

### Hover-lift
```css
.n{transition:all .18s}
.n:hover{transform:scale(1.05) translateY(-2px);z-index:5;box-shadow:0 8px 24px rgba(0,0,0,.5)}
```

## Interactivity recipes

### Hover-to-explain tooltip (CSS only, no JS)
Built into the canonical template — `.t` is hidden, `.n:hover .t` shows it.

### Click-to-pin tooltip (one line of JS)
Built into the canonical template — `[data-p]` badge toggles `.p` class on the node, `.n.p .t` keeps tooltip visible.

### Tab / view toggle inside a diagram
```html
<button data-tab="overview">Overview</button>
<button data-tab="details">Details</button>
<div data-panel="overview">...</div>
<div data-panel="details" style="display:none">...</div>
<script>
  root.querySelectorAll("[data-tab]").forEach(btn => btn.addEventListener("click", () => {
    root.querySelectorAll("[data-panel]").forEach(p => p.style.display = "none");
    root.querySelector(`[data-panel="${btn.dataset.tab}"]`).style.display = "block";
  }));
</script>
```

### Click any node to wake AI (full round-trip)
Add `data-click="node-name"` + use `on_click_inject_context` on the open_workshop call. See the `immorterm-workshop-wake-on-click` skill.

## Sizing rules

- **Inline (scroll-anchored)**: max `width:720px height:460px`. Larger competes with conversation flow.
- **Workshop panel**: width:100%, height up to 80vh — gets the whole right column.
- **Text sizes**: title 13-14px / 700, node label 12-13px / 700, body 10.5-11px, captions 9-10px / muted color
- **Padding**: card 14px, node 10-12px
- **Border radius**: card 10-12px, node 8-10px, badges 10-12px (pill)
- **NEVER** ship text smaller than 9px — unreadable on most displays

## Common gotchas

- **Shadow DOM**: scripts get `root` (shadow root), NOT `document`. Use `root.querySelector(...)` always. `document.head` IS usable for dynamic script injection, but your content lives under `root`.
- **Inline event handlers** (onclick="..." in HTML): work in draw_html but verbose. Use `addEventListener` via `<script>` for anything beyond toggling one class.
- **`new Function()` / `eval`** allowed by CSP, but never interpolate user-typed strings into JS — use `JSON.stringify(userInput)` to escape.
- **External libraries via CDN**: works for most via `document.createElement('script') + appendChild`. Mermaid loads but renders ugly. Chart.js works well. D3 works. Avoid Mermaid.
- **MCP transport string limit**: ~8KB per `draw_html` call (JSON string limit). Keep diagrams under 6KB of HTML; assets that need to be larger should be authored in pieces via `eval_in_workshop` mutations after the initial draw.
- **Max ~64KB per overlay body** before performance degrades.

## When to drop to SVG

Use SVG only when:
1. You need precise geometric shapes (gauges, ring/donut charts, custom curved paths, gradients between specific points)
2. AND you don't need rich interactivity (HTML hover/click is easier)
3. Always set explicit `width` and `height` attributes on `<svg>` — without them it collapses to 0×0 in our Shadow DOM mount
4. If still rendering wrong, wrap in `<img src="data:image/svg+xml;base64,...">` — the image decoder is more forgiving than the HTML DOMParser

## When to drop to Mermaid

Never. Renders cramped boxes with sparse layout. The HTML template above produces a richer result with one-tenth the visual real estate wasted.

## When to drop to ASCII

Never. Catppuccin-themed HTML cards are always better.
