//! ImmorTerm-specific browser mirroring — the consumer-side skin over rudder.
//!
//! rudder (the extracted browser driver) draws nothing itself; it only streams
//! frames + a mascot-neutral cursor/narration protocol. This module holds the
//! bits that are ImmorTerm's own: rendering a screenshot into an AI-canvas HTML
//! overlay. Re-homed here when the browser driver moved to the `rudder` crate.

/// Render a browser screenshot as a captioned HTML card for the AI canvas.
///
/// Emitting a `data:` image URI inside a DrawHtml overlay (rather than a
/// dedicated `browser_frame` WS message + webview renderer, which don't exist
/// yet) already satisfies the no-disk ephemerality requirement — unlike
/// Workshops, which persist. Upgrade to a dedicated browser_frame message only
/// if a live-video mirror (dropping stale frames) is needed.
pub fn mirror_html(png_base64: &str, title: &str, url: &str) -> String {
    let caption = format!("🌐 {title} — {url}");
    let safe = caption
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let data_uri = format!("data:image/png;base64,{png_base64}");
    format!(
        "<div style=\"width:100%;background:#11111b;border:1px solid #585b70;\
         border-radius:6px;overflow:hidden;font-family:monospace\">\
         <div style=\"padding:4px 8px;font-size:11px;color:#cdd6f4;\
         background:#181825;white-space:nowrap;overflow:hidden;\
         text-overflow:ellipsis\">{safe}</div>\
         <img src=\"{data_uri}\" style=\"display:block;width:100%;height:auto\"/></div>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_html_embeds_data_uri_and_caption() {
        let html = mirror_html("QUJD", "Example", "https://example.com");
        assert!(html.contains("data:image/png;base64,QUJD"));
        assert!(html.contains("Example"));
        assert!(html.contains("example.com"));
    }
}
