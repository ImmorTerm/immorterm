//! GPU-accelerated terminal renderer via wgpu.
//!
//! Platform-agnostic: receives a wgpu Device + Queue from the host layer
//! (native winit or WASM canvas) and renders terminal content. Does NOT
//! depend on winit or any platform I/O.

pub mod atlas;
pub mod bidi;
pub mod images;
pub mod panes;
pub mod pipeline;
pub mod popup;
pub mod renderer;
pub mod statusbar;
pub mod theme;

pub use atlas::{CellMetrics, FallbackGlyph, GlyphAtlas, MonoGlyph};
pub use images::{ImageInstance, ImageRenderer};
pub use panes::{PaneLayout, PaneRect};
pub use pipeline::{BgInstance, DecorInstance, GlyphInstance, TextPipeline, Uniforms};
pub use popup::{PopupAction, PopupRenderData, PopupRenderItem};
pub use renderer::{PaneChrome, PaneRegion, RenderOptions, Selection, TerminalRenderer};
pub use statusbar::{AiStatsMode, StatusBarData, StatusBarTarget, StatusBarTheme, THEME_PRESETS};
pub use bidi::{BidiRowCache, ParagraphDirection, TextAlignment};
pub use theme::Theme;

#[cfg(test)]
mod shader_validation;
