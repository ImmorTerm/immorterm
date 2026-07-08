pub mod ai_layer;
pub mod cursor_nav;
pub mod cell;
pub mod cursor;
pub mod diagram;
pub mod expression;
pub mod graphics;
pub mod marker;
pub mod grid;
pub mod links;
pub mod log;
pub mod overlays;
pub mod scrollback;
pub mod session;
pub mod subagent;
pub mod team;
pub mod terminal;

pub use ai_layer::AiLayerState;
pub use cell::{Cell, CellAttrs, Color};
pub use expression::{ExpressionMeta, ExpressionState};
pub use cursor::Cursor;
pub use grid::{Grid, Row};
pub use log::{
    AttributeRun, GridSnapshot, PromptEvent, RowRuns, ScrollbackDump, ScrollbackLine,
    SnapshotTrigger, runs_to_row,
};
pub use overlays::OverlayState;
pub use scrollback::Scrollback;
pub use team::{TeamConfig, TeamLifecycle, TeamMember, TeamState, TeamTask};
pub use terminal::{CombiningMarks, PromptState, Terminal, TerminalSnapshot};

/// Serde helper: default value `true` for fields that should be true when missing from old snapshots.
pub fn serde_true() -> bool {
    true
}
