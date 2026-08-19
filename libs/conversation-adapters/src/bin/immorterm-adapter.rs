use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use conversation_adapters::parse_turns_from_text;
use conversation_adapters::turn::{BlockKind, Turn};

const MAX_INPUT_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "immorterm-adapter",
    version,
    about = "Normalize AI transcripts"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Normalize {
        path: PathBuf,
        #[arg(long, value_enum, default_value_t = OutputFormat::Jsonl)]
        format: OutputFormat,
        #[arg(long, default_value_t = 0)]
        byte_offset: u64,
        #[arg(long, default_value_t = 30_000)]
        max_total: usize,
        #[arg(long, default_value_t = 2_000)]
        max_per_msg: usize,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Jsonl,
    Digest,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Command::Normalize {
            path,
            format,
            byte_offset,
            max_total,
            max_per_msg,
        } => {
            let text = read_increment(&path, byte_offset)?;
            let (turns, tool) = parse_turns_from_text(&text);
            match format {
                OutputFormat::Jsonl => {
                    for event in conversation_adapters::turn::turns_to_events(&turns, tool, "", "")
                    {
                        println!("{}", serde_json::to_string(&event)?);
                    }
                }
                OutputFormat::Digest => print!("{}", render_digest(&turns, max_total, max_per_msg)),
            }
        }
    }
    Ok(())
}

/// Read only the new transcript range. Very large first-run transcripts are
/// capped to their newest 32 MiB so recovery cannot allocate gigabytes just to
/// build a 30 KiB digest prompt. Start on a complete JSONL record.
fn read_increment(path: &Path, byte_offset: u64) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let requested = byte_offset.min(size);
    let start = requested.max(size.saturating_sub(MAX_INPUT_BYTES));
    file.seek(SeekFrom::Start(start))?;

    let mut bytes = Vec::with_capacity((size - start).min(MAX_INPUT_BYTES) as usize);
    file.read_to_end(&mut bytes)?;
    if start > 0 {
        if let Some(newline) = bytes.iter().position(|b| *b == b'\n') {
            bytes.drain(..=newline);
        } else {
            bytes.clear();
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn render_digest(turns: &[Turn], max_total: usize, max_per_msg: usize) -> String {
    let mut messages = Vec::new();
    let mut total = 0usize;
    for turn in turns {
        push_message(
            &mut messages,
            &mut total,
            max_total,
            max_per_msg,
            "User",
            &turn.user_text,
        );

        let mut parts = Vec::new();
        for block in &turn.blocks {
            match block.kind {
                BlockKind::Text => parts.push(block.text.clone()),
                BlockKind::Thinking => {}
                BlockKind::ToolUse => {
                    if let Some(call) = &block.tool_call {
                        let detail = call
                            .input
                            .get("file_path")
                            .or_else(|| call.input.get("command"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if detail.is_empty() {
                            parts.push(format!("[Tool: {}]", call.name));
                        } else {
                            parts.push(format!("[Tool: {} {}]", call.name, truncate(detail, 120)));
                        }
                        if let Some(result) = &call.result
                            && !result.trim().is_empty()
                        {
                            parts.push(format!(
                                "[Result: {}]",
                                truncate(&result.replace('\n', " "), 160)
                            ));
                        }
                    }
                }
            }
        }
        push_message(
            &mut messages,
            &mut total,
            max_total,
            max_per_msg,
            "AI",
            &parts.join(" "),
        );
        if total >= max_total {
            break;
        }
    }
    messages.join("\n\n")
}

fn push_message(
    out: &mut Vec<String>,
    total: &mut usize,
    max_total: usize,
    max_per_msg: usize,
    label: &str,
    text: &str,
) {
    let clean = text.trim();
    if clean.is_empty() || *total >= max_total {
        return;
    }
    let remaining = max_total - *total;
    let body = truncate(clean, max_per_msg.min(remaining));
    if body.is_empty() {
        return;
    }
    *total += body.len();
    out.push(format!("{label}: {body}"));
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_output_uses_labels_expected_by_hook() {
        let turns = vec![Turn {
            index: 1,
            user_text: "Fix it".into(),
            blocks: vec![conversation_adapters::turn::AssistantBlock::text(
                "Done", None,
            )],
            timestamp: String::new(),
            system_events: vec![],
        }];
        assert_eq!(
            render_digest(&turns, 30_000, 2_000),
            "User: Fix it\n\nAI: Done"
        );
    }
}
