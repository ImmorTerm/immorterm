//! Overlay state — annotations and charts rendered on top of terminal content.
//!
//! These are stored as terminal state (not renderer state) so they persist
//! across redraws and can be manipulated via IPC/MCP tools.

/// A highlighted region with a label, rendered as colored borders.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Annotation {
    pub id: u32,
    /// Column position (0-indexed)
    pub col: usize,
    /// Absolute row position (scrollback.len() + grid_row)
    pub row: usize,
    /// Width in columns
    pub width: usize,
    /// Height in rows
    pub height: usize,
    /// Border color [R, G, B, A] (0.0-1.0)
    pub color: [f32; 4],
    /// Label text displayed above the region
    pub label: String,
}

/// A chart overlay (sparkline or bar chart).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChartOverlay {
    pub id: u32,
    /// Column position (0-indexed)
    pub col: usize,
    /// Absolute row position (scrollback.len() + grid_row)
    pub row: usize,
    /// Width in columns
    pub width: usize,
    /// Height in rows
    pub height: usize,
    /// Data values (normalized 0.0-1.0 for rendering)
    pub values: Vec<f32>,
    /// Chart color [R, G, B, A]
    pub color: [f32; 4],
    /// Chart type
    pub chart_type: ChartType,
}

/// Supported chart types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChartType {
    Sparkline,
    Bar,
}

/// Container for all overlay state in a terminal.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OverlayState {
    pub annotations: Vec<Annotation>,
    pub charts: Vec<ChartOverlay>,
    next_id: u32,
    /// Set to true when overlays change (renderer can check and reset).
    pub dirty: bool,
}

impl OverlayState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an annotation and return its ID.
    pub fn add_annotation(
        &mut self,
        col: usize,
        row: usize,
        width: usize,
        height: usize,
        color: [f32; 4],
        label: String,
    ) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.annotations.push(Annotation {
            id,
            col,
            row,
            width,
            height,
            color,
            label,
        });
        self.dirty = true;
        id
    }

    /// Add a chart overlay and return its ID.
    #[allow(clippy::too_many_arguments)]
    pub fn add_chart(
        &mut self,
        col: usize,
        row: usize,
        width: usize,
        height: usize,
        values: Vec<f32>,
        color: [f32; 4],
        chart_type: ChartType,
    ) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.charts.push(ChartOverlay {
            id,
            col,
            row,
            width,
            height,
            values,
            color,
            chart_type,
        });
        self.dirty = true;
        id
    }

    /// Remove an overlay (annotation or chart) by ID.
    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.annotations.len() + self.charts.len();
        self.annotations.retain(|a| a.id != id);
        self.charts.retain(|c| c.id != id);
        let after = self.annotations.len() + self.charts.len();
        if before != after {
            self.dirty = true;
            true
        } else {
            false
        }
    }

    /// Clear all overlays.
    pub fn clear(&mut self) {
        if !self.annotations.is_empty() || !self.charts.is_empty() {
            self.dirty = true;
        }
        self.annotations.clear();
        self.charts.clear();
    }
}
