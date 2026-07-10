# AI Canvas — Drawing on the ImmorTerm Terminal

Guide for using the ImmorTerm MCP drawing tools (`draw_html`, `draw_rect`, `draw_text`, `draw_button`, `draw_line`) to render overlays on the GPU terminal.

## Architecture: Two Rendering Paths

| Path | Primitives | Rendered by | Coordinate space |
|------|-----------|-------------|-----------------|
| **GPU** | `rect`, `text`, `line` | WebGPU shader | Physical pixels |
| **DOM** | `html`, `button` | Shadow DOM in VS Code webview | Physical pixels (x/y) → CSS pixels via DPR division |

GPU primitives are pixel-perfect and fast. DOM primitives support full HTML/CSS and interactivity (clicks).

## draw_html — The Main Tool

`draw_html` renders arbitrary HTML/CSS inside an isolated Shadow DOM overlay.

### Simplest usage — just pass HTML

```python
# Auto-centers on screen, auto-sizes from content, fixed anchor
draw_html(html='<div style="padding:20px;background:#1e1e2e;color:white;border-radius:8px">Hello!</div>')
```

**Everything is optional except `html`.** Defaults:
- `x`/`y`: omit → auto-centers on screen
- `width`/`height`: omit → auto-sizes from Shadow DOM content (recommended)
- `anchor`: defaults to `fixed` (pinned to screen)

### Positioning

| Approach | When to use |
|----------|------------|
| **Omit x/y** (auto-center) | Default — cards, dialogs, interactive panels |
| **Pass x/y** (physical pixels) | Edge-relative positioning — call `get_viewport` first |

When providing x/y, they are in **physical pixels** (same as GPU primitives). The frontend divides by DPR to convert to CSS position.

### Sizing — always let content determine size

Omit `width`/`height`. Define dimensions in your CSS instead:

```python
# CORRECT — CSS controls size, container auto-fits
draw_html(
    html='<div class="card">Content</div>',
    css='.card { width: 300px; padding: 20px; background: #1e1e2e; }'
)

# WRONG — explicit width/height can cause sizing mismatches
draw_html(html='...', css='...', width=300, height=200)
```

### Anchor modes

| Anchor | Behavior | Use for |
|--------|----------|---------|
| `fixed` (default) | Pinned to screen | Persistent UI: buttons, dialogs, HUDs |
| `scroll` | Moves with terminal content | Inline annotations on specific output lines |

**Only use `scroll` for annotations.** Scroll-anchored elements become invisible when new terminal output pushes them above the viewport.

### Interactive HTML — Self-Contained Mini-Apps

DrawHtml supports full interactivity via inline event handlers and `<script>` tags. The AI generates complete self-contained apps — popovers, modals, accordions, animations — with **zero round-trip** to the AI agent.

#### Inline event handlers

```python
# Clickable text that changes color
draw_html(html='<span onclick="this.style.color=\'#ff0\'" style="cursor:pointer;color:#fff">Click me!</span>')

# Hover tooltip
draw_html(html='''
  <span onmouseenter="this.nextElementSibling.style.display='block'"
        onmouseleave="this.nextElementSibling.style.display='none'"
        style="cursor:pointer;color:#b482ff">hover here</span>
  <div style="display:none;background:#1e1e2e;border:1px solid #b482ff;padding:8px;border-radius:4px;color:#fff;margin-top:4px">
    Tooltip content!
  </div>
''')
```

#### Script tags with shadow root access

Scripts receive context variables: `root` (shadow root), `wrapper` (content div), `card` (overlay element), `prim` (primitive data).

```python
draw_html(html='''
  <button id="btn" style="padding:12px 24px;background:#6366f1;color:white;border:none;border-radius:8px;cursor:pointer">
    Open Modal
  </button>
  <div id="modal" style="display:none;position:fixed;inset:0;background:rgba(0,0,0,0.5);z-index:100">
    <div style="position:absolute;top:50%;left:50%;transform:translate(-50%,-50%);background:#1e1e2e;padding:24px;border-radius:12px;color:white;min-width:300px">
      <h3 style="margin:0 0 12px">Modal Title</h3>
      <p>Self-contained modal — no AI round-trip needed!</p>
      <button id="close" style="margin-top:12px;padding:8px 16px;background:#ef4444;color:white;border:none;border-radius:6px;cursor:pointer">Close</button>
    </div>
  </div>
  <script>
    const btn = root.getElementById('btn');
    const modal = root.getElementById('modal');
    const close = root.getElementById('close');
    btn.onclick = () => modal.style.display = 'block';
    close.onclick = () => modal.style.display = 'none';
    modal.onclick = (e) => { if (e.target === modal) modal.style.display = 'none'; };
  </script>
''')
```

#### Key rules for scripts

- Use `root.getElementById()` / `root.querySelector()` — NOT `document.*` (Shadow DOM isolation)
- Scripts run once when the primitive is created (not on re-sync)
- Errors are caught and logged: `[AI HTML] Script error in primitive N: ...`
- CSS animations, transitions, and keyframes all work inside Shadow DOM

### data-click — AI-Responsive Events (Optional)

For interactions that need the AI to respond dynamically, use `data-click`:

```python
draw_html(
    html='<button data-click="confirm" style="cursor:pointer;padding:12px 24px">Confirm</button>',
    css='button:hover { background: #e0e7ff; }'
)

# Wait for click in background
wait_for_event(event_type="click", timeout=60)

# Later, check what was clicked
events = poll_events()  # [{ id, data_click: "confirm", ... }]
```

**When to use which:**
| Approach | Use for |
|----------|---------|
| Inline JS / `<script>` | Tooltips, modals, accordions, animations — anything self-contained |
| `data-click` + `poll_events` | Actions that need AI reasoning (e.g., "user chose option B, now do X") |
| Both together | Interactive UI with some actions that notify the AI |

### Template: Centered circle with clickable button

```python
draw_html(
    html='''
    <div class="circle">
      <button data-click="action" class="btn">CLICK ME</button>
    </div>
    ''',
    css='''
    .circle {
      width: 250px; height: 250px;
      border-radius: 50%;
      background: radial-gradient(circle, #6366f1, #4f46e5);
      display: flex; align-items: center; justify-content: center;
      box-shadow: 0 0 40px rgba(99,102,241,0.5);
    }
    .btn {
      padding: 12px 28px; border: none; border-radius: 8px;
      background: white; color: #4f46e5; font-weight: bold;
      cursor: pointer; font-size: 16px;
    }
    .btn:hover { background: #e0e7ff; }
    '''
)
# No x, y, width, or height needed — auto-centers, auto-sizes
```

## Inline `<<html>>` Blocks — Embedded Visuals in Terminal Output

Claude can emit `<<html>>` blocks directly in PTY output. The terminal parser intercepts them, strips them from visible text, and renders them as scroll-anchored overlays at the exact line where they appeared.

**No MCP tool call needed.** Just output the tags in your response and the renderer handles the rest.

### Syntax

```
<<html>>
<div style="padding:16px;background:#1e1e2e;color:#cdd6f4;border-radius:8px;border:1px solid #45475a">
  <h3 style="margin:0 0 8px;color:#b4befe">Title</h3>
  <p style="margin:0">Content here — full HTML/CSS/JS supported.</p>
</div>
<</html>>
```

### With attributes

```
<<html anchor=fixed name=my-chart>>
<canvas id="chart" width="400" height="200"></canvas>
<script>
  // Scripts get shadow root context: root, wrapper, card, prim
  const ctx = root.getElementById('chart').getContext('2d');
  // ... draw on canvas
</script>
<</html>>
```

| Attribute | Values | Default | Purpose |
|-----------|--------|---------|---------|
| `anchor` | `scroll`, `fixed` | `scroll` | `scroll` = moves with terminal content; `fixed` = pinned to viewport |
| `name` | any string | none | Named primitives can be updated/removed by name |

### What to use inline blocks for

- **Architecture diagrams** — SVG or HTML/CSS boxes with arrows
- **Data visualizations** — Chart.js charts, progress bars, metric dashboards
- **Interactive widgets** — tabbed panels, accordions, filterable tables
- **Mermaid diagrams** — render via `<script>` with Mermaid CDN
- **Code diffs** — side-by-side comparisons with syntax highlighting
- **Decision trees** — clickable flowcharts

### Key rules

1. **Always inline styles or `<style>` tags** — no external stylesheets (Shadow DOM isolation)
2. **Use `root.getElementById()`** in scripts, not `document.getElementById()` (Shadow DOM)
3. **Default anchor is `scroll`** — the block scrolls with terminal output (unlike MCP `draw_html` which defaults to `fixed`)
4. **Keep blocks self-contained** — each block is independent; they cannot reference each other
5. **Max 64KB per block** — the parser has a 64KB buffer limit
6. **Catppuccin Mocha theme** — match the terminal aesthetic: `#1e1e2e` (base), `#313244` (surface0), `#45475a` (surface1), `#cdd6f4` (text), `#b4befe` (lavender), `#a6e3a1` (green), `#f38ba8` (red)

### Example: SVG architecture diagram

```
<<html>>
<svg viewBox="0 0 500 200" style="max-width:500px;font-family:monospace">
  <rect x="10" y="60" width="120" height="50" rx="8" fill="#313244" stroke="#b4befe"/>
  <text x="70" y="90" text-anchor="middle" fill="#cdd6f4" font-size="12">Frontend</text>
  <rect x="190" y="60" width="120" height="50" rx="8" fill="#313244" stroke="#a6e3a1"/>
  <text x="250" y="90" text-anchor="middle" fill="#cdd6f4" font-size="12">API Server</text>
  <rect x="370" y="60" width="120" height="50" rx="8" fill="#313244" stroke="#f9e2af"/>
  <text x="430" y="90" text-anchor="middle" fill="#cdd6f4" font-size="12">Database</text>
  <line x1="130" y1="85" x2="190" y2="85" stroke="#585b70" stroke-width="2" marker-end="url(#arrow)"/>
  <line x1="310" y1="85" x2="370" y2="85" stroke="#585b70" stroke-width="2" marker-end="url(#arrow)"/>
  <defs><marker id="arrow" viewBox="0 0 10 10" refX="10" refY="5" markerWidth="6" markerHeight="6" orient="auto">
    <path d="M 0 0 L 10 5 L 0 10 z" fill="#585b70"/></marker></defs>
</svg>
<</html>>
```

### Example: Interactive Chart.js

```
<<html>>
<div style="background:#1e1e2e;padding:16px;border-radius:8px;border:1px solid #45475a">
  <canvas id="perf-chart" width="500" height="250"></canvas>
</div>
<script>
  const s = document.createElement('script');
  s.src = 'https://cdn.jsdelivr.net/npm/chart.js@4';
  s.onload = () => {
    const ctx = root.getElementById('perf-chart').getContext('2d');
    new Chart(ctx, {
      type: 'line',
      data: {
        labels: ['Mon','Tue','Wed','Thu','Fri'],
        datasets: [{
          label: 'Response Time (ms)',
          data: [120, 95, 140, 88, 102],
          borderColor: '#b4befe',
          backgroundColor: 'rgba(180,190,254,0.1)',
          tension: 0.3,
          fill: true
        }]
      },
      options: {
        plugins: { legend: { labels: { color: '#cdd6f4' } } },
        scales: {
          x: { ticks: { color: '#6c7086' }, grid: { color: '#313244' } },
          y: { ticks: { color: '#6c7086' }, grid: { color: '#313244' } }
        }
      }
    });
  };
  wrapper.appendChild(s);
</script>
<</html>>
```

### Inline blocks vs MCP `draw_html`

| | Inline `<<html>>` | MCP `draw_html` |
|-|-------------------|-----------------|
| **Trigger** | Claude outputs tags in response | Explicit tool call |
| **Default anchor** | `scroll` (flows with output) | `fixed` (pinned to viewport) |
| **Positioning** | Auto — at the line where tag appears | Explicit x/y or auto-center |
| **Use case** | Diagrams, charts embedded in conversation | Persistent UI, buttons, panels |
| **CSS** | Inline styles / `<style>` tags | Separate `css` parameter |

## draw_rect / draw_text / draw_line — GPU Primitives

Render directly on the WebGPU canvas. Fast, crisp, no DPR issues (GPU shader handles scaling). **Not interactive** — use `draw_html` for clickable elements.

```python
draw_rect(x=100, y=100, width=200, height=50, color="#ff0000")
draw_text(x=100, y=120, text="Hello!", color="#ffffff", font_size=16)
draw_line(x1=0, y1=100, x2=500, y2=100, color="#333333")
```

## draw_button — Simple Clickable Button

Convenience tool for a single button without writing HTML/CSS:

```python
draw_button(label="Submit", x=400, y=300, data_click="submit", anchor="fixed")
```

## Event Handling

```python
# 1. Draw interactive element (auto-centers)
draw_html(html='<button data-click="go">Go</button>', css="...")

# 2. Start background listener
wait_for_event(event_type="click", timeout=60)

# 3. Poll when ready
events = poll_events()  # returns list with { id, data_click, timestamp }
```

## Cleanup

```python
remove_primitive(id=3)   # remove one overlay
clear_ai_layer()         # remove everything
```

## Quick Reference

| Do | Don't |
|----|-------|
| Omit x/y for auto-centering | Hardcode pixel coordinates |
| Omit width/height (auto-size from CSS) | Pass explicit container dimensions |
| Use `anchor: fixed` for interactive UI | Use `anchor: scroll` for persistent widgets |
| Define sizes in CSS inside Shadow DOM | Rely on card container sizing |
| Use inline JS / `<script>` for self-contained UX | Round-trip to AI for tooltips, modals, accordions |
| Use `root.querySelector()` in scripts | Use `document.querySelector()` (Shadow DOM!) |
| Use `data-click` when AI must respond | Use `data-click` for pure UI interactions |
| Use GPU primitives for non-interactive visuals | Use `draw_html` when `draw_rect` suffices |
