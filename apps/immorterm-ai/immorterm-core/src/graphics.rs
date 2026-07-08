//! Kitty graphics protocol support.
//!
//! Handles inline image display via the Kitty graphics protocol (APC sequences).
//! Parses `ESC _ G <key=value>;...;<base64 data> ESC \` sequences, decodes image
//! data (PNG, RGB, RGBA), and stores placements for the renderer.

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How an image is placed relative to the text grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlacementMode {
    /// Inline with text, occupying cells
    Inline,
    /// Cover area (stretch to fill)
    Cover,
    /// Contain within area (maintain aspect ratio)
    Contain,
}

/// A single image placement in the terminal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePlacement {
    /// Unique image ID
    pub id: u32,
    /// RGBA pixel data
    pub data: Vec<u8>,
    /// Image width in pixels
    pub width: u32,
    /// Image height in pixels
    pub height: u32,
    /// Placement mode
    pub placement: PlacementMode,
    /// Z-index for layering
    pub z_index: i32,
    /// Grid row where the image starts
    pub row: usize,
    /// Grid column where the image starts
    pub col: usize,
    /// Width in cells
    pub cell_width: usize,
    /// Height in cells
    pub cell_height: usize,
}

/// Manages all image placements for a terminal session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphicsState {
    /// Images by their ID
    images: HashMap<u32, ImagePlacement>,
    /// Next auto-assigned image ID
    next_id: u32,
    /// Pending image data being received in chunks
    pending: Option<PendingImage>,
    /// IDs of images that were added/changed since last checked by renderer
    #[serde(skip)]
    pub dirty_ids: Vec<u32>,
    /// IDs of images that were removed since last checked by renderer
    #[serde(skip)]
    pub removed_ids: Vec<u32>,
}

/// Image data being received across multiple APC sequences.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingImage {
    id: u32,
    /// Accumulated base64 data (decoded on finalize)
    base64_data: Vec<u8>,
    width: u32,
    height: u32,
    format: ImageFormat,
    placement: PlacementParams,
}

/// Kitty image format (f= parameter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum ImageFormat {
    /// f=32: raw RGBA
    Rgba,
    /// f=24: raw RGB
    Rgb,
    /// f=100: PNG compressed
    Png,
}

/// Placement parameters extracted from the control data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PlacementParams {
    /// Display columns (c=)
    cols: Option<usize>,
    /// Display rows (r=)
    rows: Option<usize>,
    /// Z-index (z=)
    z_index: i32,
}

/// Parsed Kitty graphics command from APC sequence.
#[derive(Debug)]
struct GraphicsCommand {
    action: Action,
    image_id: Option<u32>,
    format: ImageFormat,
    width: u32,
    height: u32,
    more_chunks: bool,
    placement: PlacementParams,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// a=t or a=T: Transmit image data (T also displays)
    Transmit,
    /// a=T: Transmit and display
    TransmitAndDisplay,
    /// a=p: Display a previously transmitted image
    Display,
    /// a=d: Delete image(s)
    Delete,
    /// a=q: Query terminal capabilities
    Query,
}

impl GraphicsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get an image placement by ID.
    pub fn get(&self, id: u32) -> Option<&ImagePlacement> {
        self.images.get(&id)
    }

    /// Remove an image by ID.
    pub fn remove(&mut self, id: u32) -> Option<ImagePlacement> {
        let img = self.images.remove(&id);
        if img.is_some() {
            self.removed_ids.push(id);
        }
        img
    }

    /// Remove all images.
    pub fn clear(&mut self) {
        let ids: Vec<u32> = self.images.keys().copied().collect();
        self.removed_ids.extend(ids);
        self.images.clear();
    }

    /// Iterator over all image placements.
    pub fn placements(&self) -> impl Iterator<Item = &ImagePlacement> {
        self.images.values()
    }

    /// Number of images.
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// Whether there are no images.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// Insert a fully decoded image placement.
    pub fn insert(&mut self, placement: ImagePlacement) {
        let id = placement.id;
        self.images.insert(id, placement);
        self.dirty_ids.push(id);
    }

    /// Allocate a new image ID.
    pub fn alloc_id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    /// Drain dirty image IDs (renderer calls this after uploading).
    pub fn take_dirty(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.dirty_ids)
    }

    /// Drain removed image IDs (renderer calls this after freeing GPU textures).
    pub fn take_removed(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.removed_ids)
    }

    /// Process a Kitty graphics APC payload.
    ///
    /// `payload` is the bytes between `G` and `ESC \` in: `ESC _ G <payload> ESC \`
    /// `cursor_row` and `cursor_col` are the current cursor position for placement.
    pub fn process_command(&mut self, payload: &[u8], cursor_row: usize, cursor_col: usize) {
        let payload_str = match std::str::from_utf8(payload) {
            Ok(s) => s,
            Err(_) => return,
        };

        let cmd = match Self::parse_command(payload_str) {
            Some(cmd) => cmd,
            None => return,
        };

        match cmd.action {
            Action::Transmit => self.handle_transmit(cmd, cursor_row, cursor_col),
            Action::TransmitAndDisplay => self.handle_transmit(cmd, cursor_row, cursor_col),
            Action::Display => self.handle_display(cmd, cursor_row, cursor_col),
            Action::Delete => self.handle_delete(cmd),
            Action::Query => {} // We don't respond to queries (no PTY write-back in core)
        }
    }

    /// Parse key=value pairs and base64 payload from the APC data.
    fn parse_command(data: &str) -> Option<GraphicsCommand> {
        // Format: "key=val,key=val,...;base64data"
        // The semicolon separates control keys from payload
        let (control, payload_b64) = match data.find(';') {
            Some(pos) => (&data[..pos], &data[pos + 1..]),
            None => (data, ""),
        };

        let mut action = Action::TransmitAndDisplay; // Default: transmit+display
        let mut image_id: Option<u32> = None;
        let mut format = ImageFormat::Rgba;
        let mut width: u32 = 0;
        let mut height: u32 = 0;
        let mut more_chunks = false;
        let mut placement = PlacementParams::default();

        for pair in control.split(',') {
            let (key, val) = match pair.split_once('=') {
                Some(kv) => kv,
                None => continue,
            };

            match key {
                "a" => {
                    action = match val {
                        "t" => Action::Transmit,
                        "T" => Action::TransmitAndDisplay,
                        "p" => Action::Display,
                        "d" => Action::Delete,
                        "q" => Action::Query,
                        _ => Action::TransmitAndDisplay,
                    };
                }
                "i" => image_id = val.parse().ok(),
                "f" => {
                    format = match val {
                        "24" => ImageFormat::Rgb,
                        "32" => ImageFormat::Rgba,
                        "100" => ImageFormat::Png,
                        _ => ImageFormat::Rgba,
                    };
                }
                "s" => width = val.parse().unwrap_or(0),
                "v" => height = val.parse().unwrap_or(0),
                "m" => more_chunks = val == "1",
                "c" => placement.cols = val.parse().ok(),
                "r" => placement.rows = val.parse().ok(),
                "z" => placement.z_index = val.parse().unwrap_or(0),
                _ => {} // Ignore unknown keys (t=, o=, X=, Y=, etc.)
            }
        }

        // Decode base64 payload
        let payload = if payload_b64.is_empty() {
            Vec::new()
        } else {
            base64::engine::general_purpose::STANDARD
                .decode(payload_b64)
                .unwrap_or_default()
        };

        Some(GraphicsCommand {
            action,
            image_id,
            format,
            width,
            height,
            more_chunks,
            placement,
            payload,
        })
    }

    fn handle_transmit(&mut self, cmd: GraphicsCommand, cursor_row: usize, cursor_col: usize) {
        // If no explicit image_id and there's a pending transfer, continue it.
        // This fixes multi-chunk transfers where only the first chunk has i=<id>.
        let id = if let (None, Some(pending)) = (&cmd.image_id, &self.pending) {
            pending.id
        } else {
            cmd.image_id.unwrap_or_else(|| self.alloc_id())
        };

        if cmd.more_chunks {
            // Start or continue a multi-chunk transfer
            match &mut self.pending {
                Some(pending) if pending.id == id => {
                    pending.base64_data.extend_from_slice(&cmd.payload);
                }
                _ => {
                    self.pending = Some(PendingImage {
                        id,
                        base64_data: cmd.payload,
                        width: cmd.width,
                        height: cmd.height,
                        format: cmd.format,
                        placement: cmd.placement,
                    });
                }
            }
            return;
        }

        // Final chunk (or single-chunk transfer)
        let (raw_data, width, height, format, placement) = if let Some(mut pending) = self.pending.take() {
            if pending.id == id {
                pending.base64_data.extend_from_slice(&cmd.payload);
                let w = if cmd.width > 0 { cmd.width } else { pending.width };
                let h = if cmd.height > 0 { cmd.height } else { pending.height };
                (pending.base64_data, w, h, pending.format, pending.placement)
            } else {
                // Mismatched ID — discard pending, use this chunk alone
                (cmd.payload, cmd.width, cmd.height, cmd.format, cmd.placement)
            }
        } else {
            (cmd.payload, cmd.width, cmd.height, cmd.format, cmd.placement)
        };

        if raw_data.is_empty() {
            return;
        }

        // Decode to RGBA
        let (rgba, img_w, img_h) = match Self::decode_image(&raw_data, format, width, height) {
            Some(result) => result,
            None => return,
        };

        // Calculate cell dimensions if not specified
        // Assume 8x16 cell size as fallback (renderer will scale properly)
        let cell_w = placement.cols.unwrap_or_else(|| ((img_w as f64 / 8.0).ceil() as usize).max(1));
        let cell_h = placement.rows.unwrap_or_else(|| ((img_h as f64 / 16.0).ceil() as usize).max(1));

        let img = ImagePlacement {
            id,
            data: rgba,
            width: img_w,
            height: img_h,
            placement: PlacementMode::Contain,
            z_index: placement.z_index,
            row: cursor_row,
            col: cursor_col,
            cell_width: cell_w,
            cell_height: cell_h,
        };

        self.insert(img);
    }

    fn handle_display(&mut self, cmd: GraphicsCommand, cursor_row: usize, cursor_col: usize) {
        if let Some(id) = cmd.image_id {
            // Update position of existing image
            if let Some(img) = self.images.get_mut(&id) {
                img.row = cursor_row;
                img.col = cursor_col;
                if let Some(cols) = cmd.placement.cols {
                    img.cell_width = cols;
                }
                if let Some(rows) = cmd.placement.rows {
                    img.cell_height = rows;
                }
                img.z_index = cmd.placement.z_index;
                self.dirty_ids.push(id);
            }
        }
    }

    fn handle_delete(&mut self, cmd: GraphicsCommand) {
        if let Some(id) = cmd.image_id {
            self.remove(id);
        } else {
            // No ID specified — delete all
            self.clear();
        }
    }

    /// Decode raw image data to RGBA pixels.
    fn decode_image(data: &[u8], format: ImageFormat, width: u32, height: u32) -> Option<(Vec<u8>, u32, u32)> {
        match format {
            ImageFormat::Png => {
                let img = image::load_from_memory(data).ok()?;
                let rgba = img.to_rgba8();
                Some((rgba.to_vec(), rgba.width(), rgba.height()))
            }
            ImageFormat::Rgba => {
                if width == 0 || height == 0 {
                    return None;
                }
                let expected = (width * height * 4) as usize;
                if data.len() < expected {
                    return None;
                }
                Some((data[..expected].to_vec(), width, height))
            }
            ImageFormat::Rgb => {
                if width == 0 || height == 0 {
                    return None;
                }
                let expected = (width * height * 3) as usize;
                if data.len() < expected {
                    return None;
                }
                // Convert RGB → RGBA
                let mut rgba = Vec::with_capacity((width * height * 4) as usize);
                for pixel in data[..expected].chunks_exact(3) {
                    rgba.extend_from_slice(pixel);
                    rgba.push(255);
                }
                Some((rgba, width, height))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_transmit_png() {
        let mut gs = GraphicsState::new();
        // Minimal PNG (1x1 red pixel) in base64
        let payload = b"a=T,f=100,i=1;iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==";
        gs.process_command(payload, 0, 0);
        assert!(gs.get(1).is_some());
        let img = gs.get(1).unwrap();
        assert_eq!(img.width, 1);
        assert_eq!(img.height, 1);
        assert_eq!(img.data.len(), 4); // 1x1 RGBA
    }

    #[test]
    fn parse_transmit_rgba() {
        let mut gs = GraphicsState::new();
        // 2x2 RGBA image: 16 bytes, base64 encoded
        let rgba_data = [255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&rgba_data);
        let payload = format!("a=T,f=32,s=2,v=2,i=5;{}", b64);
        gs.process_command(payload.as_bytes(), 3, 10);
        let img = gs.get(5).unwrap();
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 2);
        assert_eq!(img.row, 3);
        assert_eq!(img.col, 10);
        assert_eq!(img.data, rgba_data);
    }

    #[test]
    fn multi_chunk_transfer() {
        let mut gs = GraphicsState::new();
        let rgba_data = [255u8, 0, 0, 255]; // 1x1
        let b64 = base64::engine::general_purpose::STANDARD.encode(&rgba_data);
        let (chunk1, chunk2) = b64.split_at(b64.len() / 2);

        // First chunk with m=1 (more data coming)
        let payload1 = format!("a=T,f=32,s=1,v=1,i=7,m=1;{}", chunk1);
        gs.process_command(payload1.as_bytes(), 0, 0);
        assert!(gs.get(7).is_none()); // Not finalized yet

        // Final chunk with m=0 (implicit)
        let payload2 = format!("a=T,f=32,i=7;{}", chunk2);
        gs.process_command(payload2.as_bytes(), 1, 2);
        assert!(gs.get(7).is_some());
    }

    #[test]
    fn delete_image() {
        let mut gs = GraphicsState::new();
        gs.insert(ImagePlacement {
            id: 1,
            data: vec![255, 0, 0, 255],
            width: 1,
            height: 1,
            placement: PlacementMode::Inline,
            z_index: 0,
            row: 0,
            col: 0,
            cell_width: 1,
            cell_height: 1,
        });
        assert_eq!(gs.len(), 1);

        gs.process_command(b"a=d,i=1", 0, 0);
        assert_eq!(gs.len(), 0);
        assert_eq!(gs.take_removed(), vec![1]);
    }

    #[test]
    fn delete_all() {
        let mut gs = GraphicsState::new();
        for i in 1..=3 {
            gs.insert(ImagePlacement {
                id: i,
                data: vec![0; 4],
                width: 1,
                height: 1,
                placement: PlacementMode::Inline,
                z_index: 0,
                row: 0,
                col: 0,
                cell_width: 1,
                cell_height: 1,
            });
        }
        gs.take_dirty(); // clear dirty
        gs.process_command(b"a=d", 0, 0);
        assert!(gs.is_empty());
        assert_eq!(gs.take_removed().len(), 3);
    }
}
