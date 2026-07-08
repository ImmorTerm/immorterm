//! AI Canvas Layer — persistent drawing primitives manipulated by AI agents.
//!
//! The AI layer sits ON TOP of terminal content, rendered using the same GPU
//! pipelines (BG/Glyph/Decor). Primitives are stored here in the core crate
//! (WASM-safe, no platform deps) so they:
//!   - Survive session reconnects
//!   - Appear in screenshots
//!   - Work in WASM builds
//!
//! The renderer maps each primitive to existing GPU instances:
//!   AiRect   → BgInstance (fill) + DecorInstance (border)
//!   AiText   → GlyphInstance via render_text_at()
//!   AiButton → BgInstance + GlyphInstance + DecorInstance
//!   AiLine   → DecorInstance

use std::collections::VecDeque;

/// Positioning mode for AI primitives.
///
/// `Fixed` (default) — primitive stays at its pixel coordinates regardless of scrolling.
/// `Scroll` — primitive moves with terminal content; its y position is adjusted based on
/// how many new scrollback lines appeared since creation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq)]
pub enum AnchorMode {
    #[default]
    #[serde(rename = "fixed")]
    Fixed,
    #[serde(rename = "scroll")]
    Scroll {
        /// Scrollback length when primitive was created — used to compute scroll delta.
        scrollback_at_creation: usize,
    },
}

/// Root container for all AI-drawn primitives and animations.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AiLayerState {
    pub primitives: Vec<AiPrimitive>,
    pub animations: Vec<AiAnimation>,
    /// Events consumed by the frame tick for viewport_diff (drained every 16ms).
    pub events: VecDeque<AiEvent>,
    /// Events accumulated for MCP poll_events (survives frame ticks, drained only by MCP).
    pub mcp_events: VecDeque<AiEvent>,
    /// Next unique ID for primitives.
    next_id: u32,
    /// Set when primitives change — renderer checks and resets.
    pub dirty: bool,
}

/// A drawable primitive on the AI canvas.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiPrimitive {
    pub id: u32,
    pub kind: AiPrimitiveKind,
    /// Alpha (0.0 = invisible, 1.0 = fully opaque). Animated by the `Alpha` property.
    pub alpha: f32,
    /// Whether this primitive is visible.
    pub visible: bool,
    /// Positioning mode: fixed (HUD-like) or scroll (moves with content).
    #[serde(default)]
    pub anchor: AnchorMode,
    /// Optional human-readable name for event matching (e.g., "approve-btn", "design-a").
    /// AI assigns this when drawing; used by WaitForAiEvent to match on name instead of numeric ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Primitive types — each maps to existing GPU pipeline instances.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AiPrimitiveKind {
    Rect(AiRect),
    Text(AiText),
    Button(AiButton),
    Line(AiLine),
    Html(AiHtml),
}

/// Filled rectangle with optional border.
/// Rendered as: BgInstance (fill) + up to 4 DecorInstance (border sides).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiRect {
    /// X position in pixels.
    pub x: f32,
    /// Y position in pixels.
    pub y: f32,
    /// Width in pixels.
    pub width: f32,
    /// Height in pixels.
    pub height: f32,
    /// Fill color [R, G, B, A].
    pub color: [f32; 4],
    /// Optional border color [R, G, B, A].
    pub border_color: Option<[f32; 4]>,
    /// Border width in pixels (default 0 = no border).
    pub border_width: f32,
}

/// Text rendered at pixel coordinates.
/// Rendered as: GlyphInstance sequence via render_text_at().
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiText {
    /// The text string to render.
    pub text: String,
    /// X position in pixels.
    pub x: f32,
    /// Y position in pixels.
    pub y: f32,
    /// Text color [R, G, B, A].
    pub color: [f32; 4],
    /// Font size scale relative to base (1.0 = normal).
    pub font_size_scale: f32,
}

/// Clickable button with hover state.
/// Rendered as: BgInstance (fill) + GlyphInstance (text) + DecorInstance (border).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiButton {
    /// Button label text.
    pub text: String,
    /// X position in pixels.
    pub x: f32,
    /// Y position in pixels.
    pub y: f32,
    /// Width in pixels.
    pub width: f32,
    /// Height in pixels.
    pub height: f32,
    /// Background color [R, G, B, A].
    pub bg_color: [f32; 4],
    /// Text color [R, G, B, A].
    pub text_color: [f32; 4],
    /// Whether the mouse is currently hovering over this button.
    pub hovered: bool,
}

/// A line between two points.
/// Rendered as: DecorInstance (thin rectangle approximation).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiLine {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    /// Line color [R, G, B, A].
    pub color: [f32; 4],
    /// Line thickness in pixels.
    pub thickness: f32,
}

/// HTML/CSS component rendered as a DOM overlay (no GPU rendering).
/// Use this for rich UI: cards, forms, tables, images — anything HTML can do.
/// The webview renders this as a real DOM element with full CSS support.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiHtml {
    /// HTML content to render in the DOM overlay.
    pub html: String,
    /// CSS styles (scoped to this primitive's wrapper).
    pub css: String,
    /// X position in pixels.
    pub x: f32,
    /// Y position in pixels.
    pub y: f32,
    /// Width in pixels (0 = auto-size from content).
    pub width: f32,
    /// Height in pixels (0 = auto-size from content).
    pub height: f32,
    /// Terminal row for inline ```im-html fenced blocks. When set, the frontend
    /// computes `y = anchor_row * cellHeight * dpr` for pixel-accurate placement.
    /// `None` for MCP-created primitives that specify explicit x/y coordinates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_row: Option<usize>,
    /// Optional prompt template auto-written to the Claude PTY on each
    /// `data-click` button activation inside this overlay. Placeholders:
    /// `{data_click}` (the clicked button's data-click value), `{id}` (the
    /// primitive's numeric ID). When set, the daemon injects
    /// `<formatted-template>\n` into the session's PTY — Claude wakes
    /// naturally with NO background bash needed. Leave None to use the
    /// classic background `wait-event` flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_click_prompt: Option<String>,
    /// Optional rich-context template for the hook-injection path. Same
    /// placeholders as `on_click_prompt`. When set, the daemon writes a
    /// marker file the UserPromptSubmit hook reads + types a tiny trigger
    /// (".") so Claude sees the rich context as `additionalContext` without
    /// the verbose prompt appearing in the terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_click_inject_context: Option<String>,
}

// ─── Animations ──────────────────────────────────────────────────────

/// Declarative animation — the AI says "animate X over N ms", the renderer
/// interpolates at 60fps. Uses f32 time (seconds from renderer start) to
/// stay WASM-compatible (no std::time::Instant).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AiAnimation {
    /// Which primitive this animation targets.
    pub primitive_id: u32,
    /// Which property to animate.
    pub property: AnimProperty,
    /// Start value.
    pub from: f32,
    /// End value.
    pub to: f32,
    /// Duration in milliseconds.
    pub duration_ms: u32,
    /// Easing function.
    pub easing: EasingFunc,
    /// When the animation started (f32 seconds from renderer start).
    /// None = not yet started (will start on next tick).
    pub started_at: Option<f32>,
}

/// Animatable properties of primitives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AnimProperty {
    X,
    Y,
    Width,
    Height,
    Alpha,
}

/// Easing functions for smooth animation curves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EasingFunc {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

// ─── Events ──────────────────────────────────────────────────────────

/// Events generated by AI layer interactions (button clicks, hovers).
/// The AI polls these via `PollAiEvents` IPC.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AiEvent {
    ButtonClicked { id: u32, data_click: Option<String> },
    ButtonHovered { id: u32, entered: bool },
    /// Click inside a Workshop pane (persistent webview authored via
    /// `OpenWorkshop`). Workshops have no numeric primitive id — they're
    /// matched by `name` in `wait_for_event`. Carries the workshop name and
    /// the `data-click` label of the clicked element so the AI can route
    /// without a name → id lookup.
    WorkshopClicked { name: String, data_click: Option<String> },
}

// ─── Implementation ──────────────────────────────────────────────────

impl AiLayerState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a new unique primitive ID.
    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Add a rectangle and return its ID.
    pub fn add_rect(&mut self, rect: AiRect, anchor: AnchorMode, name: Option<String>) -> u32 {
        let id = self.alloc_id();
        self.primitives.push(AiPrimitive {
            id,
            kind: AiPrimitiveKind::Rect(rect),
            alpha: 1.0,
            visible: true,
            anchor,
            name,
        });
        self.dirty = true;
        id
    }

    /// Add text and return its ID.
    pub fn add_text(&mut self, text: AiText, anchor: AnchorMode, name: Option<String>) -> u32 {
        let id = self.alloc_id();
        self.primitives.push(AiPrimitive {
            id,
            kind: AiPrimitiveKind::Text(text),
            alpha: 1.0,
            visible: true,
            anchor,
            name,
        });
        self.dirty = true;
        id
    }

    /// Add a button and return its ID.
    pub fn add_button(&mut self, button: AiButton, anchor: AnchorMode, name: Option<String>) -> u32 {
        let id = self.alloc_id();
        self.primitives.push(AiPrimitive {
            id,
            kind: AiPrimitiveKind::Button(button),
            alpha: 1.0,
            visible: true,
            anchor,
            name,
        });
        self.dirty = true;
        id
    }

    /// Add a line and return its ID.
    pub fn add_line(&mut self, line: AiLine, anchor: AnchorMode, name: Option<String>) -> u32 {
        let id = self.alloc_id();
        self.primitives.push(AiPrimitive {
            id,
            kind: AiPrimitiveKind::Line(line),
            alpha: 1.0,
            visible: true,
            anchor,
            name,
        });
        self.dirty = true;
        id
    }

    /// Add an HTML component and return its ID. If a primitive with the same
    /// `name` already exists, REPLACE it in place (preserves the existing id
    /// so callers can keep referencing it; replaces html/anchor/visibility,
    /// resets alpha to 1.0). Without name dedup, repeated `draw_html` calls
    /// from interactive overlays (turn-based games, status panels) silently
    /// stacked duplicate cards on top of each other.
    pub fn add_html(&mut self, html: AiHtml, anchor: AnchorMode, name: Option<String>) -> u32 {
        if let Some(ref n) = name
            && let Some(existing) = self.primitives.iter_mut().find(|p| p.name.as_ref() == Some(n))
        {
            existing.kind = AiPrimitiveKind::Html(html);
            existing.anchor = anchor;
            existing.visible = true;
            existing.alpha = 1.0;
            self.dirty = true;
            return existing.id;
        }
        let id = self.alloc_id();
        self.primitives.push(AiPrimitive {
            id,
            kind: AiPrimitiveKind::Html(html),
            alpha: 1.0,
            visible: true,
            anchor,
            name,
        });
        self.dirty = true;
        id
    }

    /// Remove a primitive by ID. Returns true if found and removed.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.primitives.len();
        self.primitives.retain(|p| p.id != id);
        // Also remove any animations targeting this primitive
        self.animations.retain(|a| a.primitive_id != id);
        let removed = self.primitives.len() < before;
        if removed {
            self.dirty = true;
        }
        removed
    }

    /// Clear all primitives, animations, and events.
    pub fn clear(&mut self) {
        if !self.primitives.is_empty() || !self.animations.is_empty() {
            self.dirty = true;
        }
        self.primitives.clear();
        self.animations.clear();
        self.events.clear();
        self.mcp_events.clear();
    }

    /// Add an animation targeting a primitive.
    pub fn animate(
        &mut self,
        primitive_id: u32,
        property: AnimProperty,
        from: f32,
        to: f32,
        duration_ms: u32,
        easing: EasingFunc,
    ) {
        // Remove any existing animation on the same property of the same primitive
        self.animations.retain(|a| {
            !(a.primitive_id == primitive_id && a.property == property)
        });
        self.animations.push(AiAnimation {
            primitive_id,
            property,
            from,
            to,
            duration_ms,
            easing,
            started_at: None, // Will start on next tick
        });
        self.dirty = true;
    }

    /// Tick all animations forward. Called by the renderer each frame.
    ///
    /// `now` is seconds elapsed since renderer start (same as `time` in render()).
    /// Returns true if any animation was active (caller should request redraw).
    pub fn tick_animations(&mut self, now: f32) -> bool {
        if self.animations.is_empty() {
            return false;
        }

        let mut any_active = false;
        let mut completed = Vec::new();

        for (i, anim) in self.animations.iter_mut().enumerate() {
            // Start animation on first tick
            let start = *anim.started_at.get_or_insert(now);
            let elapsed_ms = ((now - start) * 1000.0).max(0.0);
            let duration = anim.duration_ms as f32;

            let t = if duration > 0.0 {
                (elapsed_ms / duration).min(1.0)
            } else {
                1.0
            };

            let eased_t = match anim.easing {
                EasingFunc::Linear => t,
                EasingFunc::EaseIn => t * t,
                EasingFunc::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
                EasingFunc::EaseInOut => {
                    if t < 0.5 {
                        2.0 * t * t
                    } else {
                        1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
                    }
                }
            };

            let value = anim.from + (anim.to - anim.from) * eased_t;

            // Apply to the target primitive
            if let Some(prim) = self.primitives.iter_mut().find(|p| p.id == anim.primitive_id) {
                match anim.property {
                    AnimProperty::Alpha => prim.alpha = value,
                    AnimProperty::X => set_primitive_x(&mut prim.kind, value),
                    AnimProperty::Y => set_primitive_y(&mut prim.kind, value),
                    AnimProperty::Width => set_primitive_width(&mut prim.kind, value),
                    AnimProperty::Height => set_primitive_height(&mut prim.kind, value),
                }
            }

            if t >= 1.0 {
                completed.push(i);
            } else {
                any_active = true;
            }
        }

        // Remove completed animations (reverse order to preserve indices)
        for &i in completed.iter().rev() {
            self.animations.remove(i);
        }

        if any_active || !completed.is_empty() {
            self.dirty = true;
        }

        any_active
    }

    /// Push a button click event (to both frame-tick and MCP queues).
    pub fn push_button_click(&mut self, id: u32, data_click: Option<String>) {
        let ev = AiEvent::ButtonClicked { id, data_click };
        self.events.push_back(ev.clone());
        self.mcp_events.push_back(ev);
    }

    /// Push a button hover event (to both frame-tick and MCP queues).
    pub fn push_button_hover(&mut self, id: u32, entered: bool) {
        let ev = AiEvent::ButtonHovered { id, entered };
        self.events.push_back(ev.clone());
        self.mcp_events.push_back(ev);
    }

    /// Drain frame-tick events (consumed every 16ms for viewport_diff).
    pub fn drain_events(&mut self) -> Vec<AiEvent> {
        self.events.drain(..).collect()
    }

    /// Drain MCP-pollable events (survives frame ticks, only consumed by poll_events).
    pub fn drain_mcp_events(&mut self) -> Vec<AiEvent> {
        self.mcp_events.drain(..).collect()
    }

    /// Take the first matching event from the MCP queue, leaving others untouched.
    /// Used by WaitForAiEvent to selectively consume only the event it's waiting for.
    /// Supports matching by event_type ("click"/"hover"), numeric primitive_id, and
    /// element name (looked up from primitives list).
    pub fn take_matching_mcp_event(
        &mut self,
        event_type: Option<&str>,
        primitive_id: Option<u32>,
        name: Option<&str>,
    ) -> Option<AiEvent> {
        let pos = self.mcp_events.iter().position(|ev| {
            // Workshop clicks have no numeric primitive id; match by name only.
            if let AiEvent::WorkshopClicked { name: ws_name, .. } = ev {
                if event_type.is_some() && event_type != Some("click") {
                    return false;
                }
                if primitive_id.is_some() {
                    return false;
                }
                if let Some(filter) = name
                    && filter != ws_name {
                        return false;
                    }
                return true;
            }
            let (ev_type, ev_id) = match ev {
                AiEvent::ButtonClicked { id, .. } => ("click", *id),
                AiEvent::ButtonHovered { id, .. } => ("hover", *id),
                AiEvent::WorkshopClicked { .. } => unreachable!("handled above"),
            };
            if event_type.is_some() && event_type != Some(ev_type) {
                return false;
            }
            if primitive_id.is_some() && primitive_id != Some(ev_id) {
                return false;
            }
            if let Some(name_filter) = name {
                let prim_name = self.primitives.iter()
                    .find(|p| p.id == ev_id)
                    .and_then(|p| p.name.as_deref());
                if prim_name != Some(name_filter) {
                    return false;
                }
            }
            true
        });
        pos.map(|i| self.mcp_events.remove(i).unwrap())
    }

    /// Compute the y-offset for a scroll-anchored primitive.
    fn anchor_y_offset(anchor: &AnchorMode, ch: f32, scroll_offset: usize, sb_len: usize) -> f32 {
        match anchor {
            AnchorMode::Fixed => 0.0,
            AnchorMode::Scroll { scrollback_at_creation } => {
                let new_lines = sb_len.saturating_sub(*scrollback_at_creation);
                let scroll_back = scroll_offset as f32 * ch;
                -(new_lines as f32 * ch) + scroll_back
            }
        }
    }

    /// Find a button primitive at the given pixel coordinates (for hit testing).
    ///
    /// `ch` = cell height, `scroll_offset` = current scroll position,
    /// `sb_len` = current scrollback length. These are needed to adjust
    /// coordinates for scroll-anchored buttons.
    pub fn button_at(&self, px: f32, py: f32, ch: f32, scroll_offset: usize, sb_len: usize) -> Option<u32> {
        for prim in self.primitives.iter().rev() {
            if !prim.visible {
                continue;
            }
            if let AiPrimitiveKind::Button(ref btn) = prim.kind {
                let adj_y = Self::anchor_y_offset(&prim.anchor, ch, scroll_offset, sb_len);
                let by = btn.y + adj_y;
                if px >= btn.x && px < btn.x + btn.width
                    && py >= by && py < by + btn.height
                {
                    return Some(prim.id);
                }
            }
        }
        None
    }

    /// Update hover state for buttons at the given pixel coordinates.
    /// Returns true if any hover state changed (requires redraw).
    pub fn update_hover(&mut self, px: f32, py: f32, ch: f32, scroll_offset: usize, sb_len: usize) -> bool {
        let mut changed = false;
        for prim in &mut self.primitives {
            if let AiPrimitiveKind::Button(ref mut btn) = prim.kind {
                let adj_y = Self::anchor_y_offset(&prim.anchor, ch, scroll_offset, sb_len);
                let by = btn.y + adj_y;
                let inside = prim.visible
                    && px >= btn.x && px < btn.x + btn.width
                    && py >= by && py < by + btn.height;
                if btn.hovered != inside {
                    let entered = inside;
                    btn.hovered = inside;
                    changed = true;
                    let ev = AiEvent::ButtonHovered {
                        id: prim.id,
                        entered,
                    };
                    self.events.push_back(ev.clone());
                    self.mcp_events.push_back(ev);
                }
            }
        }
        if changed {
            self.dirty = true;
        }
        changed
    }
}

// ─── Helpers to set properties on any primitive kind ─────────────────

fn set_primitive_x(kind: &mut AiPrimitiveKind, value: f32) {
    match kind {
        AiPrimitiveKind::Rect(r) => r.x = value,
        AiPrimitiveKind::Text(t) => t.x = value,
        AiPrimitiveKind::Button(b) => b.x = value,
        AiPrimitiveKind::Line(l) => l.x1 = value,
        AiPrimitiveKind::Html(h) => h.x = value,
    }
}

fn set_primitive_y(kind: &mut AiPrimitiveKind, value: f32) {
    match kind {
        AiPrimitiveKind::Rect(r) => r.y = value,
        AiPrimitiveKind::Text(t) => t.y = value,
        AiPrimitiveKind::Button(b) => b.y = value,
        AiPrimitiveKind::Line(l) => l.y1 = value,
        AiPrimitiveKind::Html(h) => h.y = value,
    }
}

fn set_primitive_width(kind: &mut AiPrimitiveKind, value: f32) {
    match kind {
        AiPrimitiveKind::Rect(r) => r.width = value,
        AiPrimitiveKind::Button(b) => b.width = value,
        AiPrimitiveKind::Html(h) => h.width = value,
        AiPrimitiveKind::Text(_) | AiPrimitiveKind::Line(_) => {} // no width
    }
}

fn set_primitive_height(kind: &mut AiPrimitiveKind, value: f32) {
    match kind {
        AiPrimitiveKind::Rect(r) => r.height = value,
        AiPrimitiveKind::Button(b) => b.height = value,
        AiPrimitiveKind::Html(h) => h.height = value,
        AiPrimitiveKind::Text(_) | AiPrimitiveKind::Line(_) => {} // no height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_remove_primitives() {
        let mut layer = AiLayerState::new();
        let id1 = layer.add_rect(AiRect {
            x: 10.0, y: 20.0, width: 100.0, height: 50.0,
            color: [1.0, 0.0, 0.0, 1.0],
            border_color: None, border_width: 0.0,
        }, AnchorMode::Fixed, None);
        let id2 = layer.add_text(AiText {
            text: "Hello".into(),
            x: 0.0, y: 0.0,
            color: [1.0, 1.0, 1.0, 1.0],
            font_size_scale: 1.0,
        }, AnchorMode::Fixed, None);
        assert_eq!(layer.primitives.len(), 2);
        assert!(layer.remove(id1));
        assert_eq!(layer.primitives.len(), 1);
        assert_eq!(layer.primitives[0].id, id2);
    }

    #[test]
    fn clear_all() {
        let mut layer = AiLayerState::new();
        layer.add_rect(AiRect {
            x: 0.0, y: 0.0, width: 10.0, height: 10.0,
            color: [0.0; 4], border_color: None, border_width: 0.0,
        }, AnchorMode::Fixed, None);
        layer.animate(0, AnimProperty::Alpha, 0.0, 1.0, 500, EasingFunc::Linear);
        layer.push_button_click(0, None);
        layer.clear();
        assert!(layer.primitives.is_empty());
        assert!(layer.animations.is_empty());
        assert!(layer.events.is_empty());
        assert!(layer.mcp_events.is_empty());
    }

    #[test]
    fn animation_tick() {
        let mut layer = AiLayerState::new();
        let id = layer.add_rect(AiRect {
            x: 0.0, y: 0.0, width: 100.0, height: 50.0,
            color: [1.0, 0.0, 0.0, 1.0],
            border_color: None, border_width: 0.0,
        }, AnchorMode::Fixed, None);
        layer.animate(id, AnimProperty::Alpha, 0.0, 1.0, 1000, EasingFunc::Linear);

        // First tick starts the animation
        assert!(layer.tick_animations(0.0));
        // Primitive alpha should be at 0.0 (just started)
        assert!((layer.primitives[0].alpha - 0.0).abs() < 0.01);

        // Halfway through
        assert!(layer.tick_animations(0.5));
        assert!((layer.primitives[0].alpha - 0.5).abs() < 0.01);

        // Complete
        assert!(!layer.tick_animations(1.0));
        assert!((layer.primitives[0].alpha - 1.0).abs() < 0.01);
        assert!(layer.animations.is_empty());
    }

    #[test]
    fn button_hit_testing() {
        let mut layer = AiLayerState::new();
        layer.add_button(AiButton {
            text: "Click Me".into(),
            x: 100.0, y: 200.0, width: 80.0, height: 30.0,
            bg_color: [0.2, 0.2, 0.8, 1.0],
            text_color: [1.0; 4],
            hovered: false,
        }, AnchorMode::Fixed, None);

        // Inside (ch=0, scroll_offset=0, sb_len=0 → no scroll adjustment)
        assert!(layer.button_at(120.0, 215.0, 0.0, 0, 0).is_some());
        // Outside
        assert!(layer.button_at(50.0, 215.0, 0.0, 0, 0).is_none());
        assert!(layer.button_at(120.0, 250.0, 0.0, 0, 0).is_none());
    }

    #[test]
    fn hover_state_updates() {
        let mut layer = AiLayerState::new();
        let id = layer.add_button(AiButton {
            text: "Btn".into(),
            x: 0.0, y: 0.0, width: 50.0, height: 20.0,
            bg_color: [0.5; 4],
            text_color: [1.0; 4],
            hovered: false,
        }, AnchorMode::Fixed, None);

        // Move into button
        assert!(layer.update_hover(25.0, 10.0, 0.0, 0, 0));
        if let AiPrimitiveKind::Button(ref btn) = layer.primitives[0].kind {
            assert!(btn.hovered);
        }
        // Check hover event was generated
        let events = layer.drain_events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            AiEvent::ButtonHovered { id: eid, entered } => {
                assert_eq!(*eid, id);
                assert!(*entered);
            }
            _ => panic!("Expected ButtonHovered"),
        }

        // Move out
        assert!(layer.update_hover(100.0, 100.0, 0.0, 0, 0));
        if let AiPrimitiveKind::Button(ref btn) = layer.primitives[0].kind {
            assert!(!btn.hovered);
        }
    }

    #[test]
    fn animation_replaces_same_property() {
        let mut layer = AiLayerState::new();
        let id = layer.add_rect(AiRect {
            x: 0.0, y: 0.0, width: 100.0, height: 50.0,
            color: [0.0; 4], border_color: None, border_width: 0.0,
        }, AnchorMode::Fixed, None);

        layer.animate(id, AnimProperty::X, 0.0, 100.0, 1000, EasingFunc::Linear);
        assert_eq!(layer.animations.len(), 1);

        // Adding another X animation replaces the first
        layer.animate(id, AnimProperty::X, 50.0, 200.0, 500, EasingFunc::EaseOut);
        assert_eq!(layer.animations.len(), 1);
        assert_eq!(layer.animations[0].from, 50.0);
        assert_eq!(layer.animations[0].to, 200.0);
    }

    #[test]
    fn serialization_roundtrip() {
        let mut layer = AiLayerState::new();
        layer.add_rect(AiRect {
            x: 10.0, y: 20.0, width: 100.0, height: 50.0,
            color: [1.0, 0.0, 0.0, 1.0],
            border_color: Some([0.0, 0.0, 1.0, 1.0]),
            border_width: 2.0,
        }, AnchorMode::Fixed, None);
        layer.add_button(AiButton {
            text: "OK".into(),
            x: 0.0, y: 0.0, width: 60.0, height: 24.0,
            bg_color: [0.2, 0.6, 0.2, 1.0],
            text_color: [1.0; 4],
            hovered: false,
        }, AnchorMode::Fixed, None);

        let json = serde_json::to_string(&layer).unwrap();
        let deserialized: AiLayerState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.primitives.len(), 2);
    }
}
