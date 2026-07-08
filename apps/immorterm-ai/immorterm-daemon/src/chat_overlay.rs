//! Chat overlay for interactive session channels.
//!
//! Renders a fixed-position chat panel on the AI canvas when two sessions
//! are paired via the interactive channel. Messages are displayed in a
//! scrollable list, and the overlay is updated (remove + re-add) on each
//! new message.

use immorterm_core::ai_layer::{AiHtml, AiLayerState, AnchorMode};

/// A single chat message in the overlay.
#[derive(Clone)]
pub struct ChatMessage {
    pub from: String,
    pub text: String,
    pub is_local: bool,
}

/// State for the chat overlay primitive.
pub struct ChatOverlay {
    pub primitive_id: Option<u32>,
    pub partner_name: String,
    pub messages: Vec<ChatMessage>,
}

impl ChatOverlay {
    pub fn new(partner_name: &str) -> Self {
        Self {
            primitive_id: None,
            partner_name: partner_name.to_string(),
            messages: Vec::new(),
        }
    }

    /// Build the HTML for the chat panel.
    fn build_html(&self) -> String {
        let mut msgs_html = String::new();
        for msg in &self.messages {
            let align = if msg.is_local { "right" } else { "left" };
            let bg = if msg.is_local { "#45475a" } else { "#313244" };
            let label = if msg.is_local { "You" } else { &msg.from };
            msgs_html.push_str(&format!(
                r#"<div style="text-align:{align};margin-bottom:8px">
                    <div style="font-size:10px;color:#6c7086;margin-bottom:2px">{label}</div>
                    <div style="display:inline-block;max-width:85%;background:{bg};padding:6px 10px;border-radius:8px;text-align:left;word-break:break-word">{text}</div>
                </div>"#,
                align = align,
                bg = bg,
                label = html_escape(label),
                text = html_escape(&msg.text),
            ));
        }

        let partner = html_escape(&self.partner_name);
        format!(
            r#"<div class="chat-panel">
                <div class="chat-header">
                    <span class="chat-title">🔗 {partner}</span>
                    <button class="chat-close" id="chat-dismiss">×</button>
                </div>
                <div class="chat-messages">{msgs_html}</div>
                <div class="chat-footer">Messages via Claude Code channel</div>
                <script>
                    const container = root.querySelector('.chat-messages');
                    if (container) container.scrollTop = container.scrollHeight;
                    const dismiss = root.getElementById('chat-dismiss');
                    if (dismiss) dismiss.onclick = () => card.style.display = 'none';
                </script>
            </div>"#,
            partner = partner,
            msgs_html = msgs_html,
        )
    }

    fn css() -> &'static str {
        r#"
        .chat-panel {
            width: 280px;
            max-height: 400px;
            background: #1e1e2e;
            border: 1px solid #45475a;
            border-radius: 12px;
            display: flex;
            flex-direction: column;
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
            font-size: 13px;
            color: #cdd6f4;
            box-shadow: 0 8px 32px rgba(0,0,0,0.4);
            overflow: hidden;
        }
        .chat-header {
            display: flex;
            align-items: center;
            justify-content: space-between;
            padding: 10px 14px;
            border-bottom: 1px solid #313244;
            background: #181825;
        }
        .chat-title {
            font-weight: 600;
            font-size: 12px;
        }
        .chat-close {
            background: none;
            border: none;
            color: #6c7086;
            font-size: 18px;
            cursor: pointer;
            padding: 0 4px;
            line-height: 1;
        }
        .chat-close:hover { color: #cdd6f4; }
        .chat-messages {
            flex: 1;
            overflow-y: auto;
            padding: 12px;
            min-height: 60px;
            max-height: 300px;
        }
        .chat-messages:empty::after {
            content: 'No messages yet — Claudes will coordinate here';
            color: #585b70;
            font-size: 11px;
            font-style: italic;
        }
        .chat-footer {
            padding: 6px 14px;
            border-top: 1px solid #313244;
            font-size: 10px;
            color: #585b70;
            text-align: center;
        }
        "#
    }

    /// Create or update the overlay primitive on the AI canvas.
    /// Returns the new primitive ID.
    pub fn render(&mut self, ai_layer: &mut AiLayerState) -> u32 {
        // Remove existing primitive if any
        if let Some(id) = self.primitive_id.take() {
            ai_layer.remove(id);
        }

        let html = self.build_html();
        let css = Self::css().to_string();

        let id = ai_layer.add_html(
            AiHtml {
                html,
                css,
                x: 0.0,  // 0,0 = auto-center; we'll position right-side below
                y: 0.0,
                width: 0.0,  // auto-size from content
                height: 0.0,
                anchor_row: None,
                on_click_prompt: None,
                on_click_inject_context: None,
            },
            AnchorMode::Fixed,
            Some("channel-chat".to_string()),
        );

        self.primitive_id = Some(id);
        id
    }

    /// Add a message and re-render.
    pub fn add_message(
        &mut self,
        from: &str,
        text: &str,
        is_local: bool,
        ai_layer: &mut AiLayerState,
    ) {
        self.messages.push(ChatMessage {
            from: from.to_string(),
            text: text.to_string(),
            is_local,
        });
        // Keep last 50 messages to prevent unbounded growth
        if self.messages.len() > 50 {
            self.messages.drain(..self.messages.len() - 50);
        }
        self.render(ai_layer);
    }

    /// Remove the overlay from the AI canvas.
    pub fn remove(&mut self, ai_layer: &mut AiLayerState) {
        if let Some(id) = self.primitive_id.take() {
            ai_layer.remove(id);
        }
        self.messages.clear();
    }
}

/// Minimal HTML escaping to prevent injection in chat messages.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
