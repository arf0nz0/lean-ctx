//! Detects and compresses nullclaw-style XML tool results embedded in
//! `role:"user"` messages.
//!
//! nullclaw wraps every tool batch as:
//! ```text
//! [Tool results]
//! <tool_result name="web_fetch" status="ok">
//! ... output ...
//! </tool_result>
//! ```
//! and appends a reflection prompt at the end.  These land as `role:"user"`
//! because nullclaw uses XML dispatch rather than OpenAI native tool messages,
//! so the proxy's `role == "tool"` compression loop never sees them.
//!
//! This module extracts each `<tool_result>` block, classifies it by tool name,
//! compresses non-ctx_* blocks (ctx_* tools are already compressed at the MCP
//! boundary — see #479), and preserves the reflection prompt verbatim.

use serde_json::Value;

use super::tool_kind;

/// Sentinel header nullclaw emits before the first `<tool_result>` block.
const TOOL_RESULTS_HEADER: &str = "[Tool results]";

/// Opening tag of a tool-result block: `<tool_result name="..."`.
const BLOCK_OPEN: &str = "<tool_result name=\"";

/// Closing tag of a tool-result block.
const BLOCK_CLOSE: &str = "</tool_result>";

/// Process all `role:"user"` messages in the array, compressing any that
/// contain nullclaw XML tool results.  Returns `true` if any message changed.
pub(super) fn compress_user_tool_results(
    messages: &mut [Value],
    live_compress: bool,
    excluded: impl Fn(&str) -> bool,
) -> bool {
    let mut modified = false;
    for msg in messages.iter_mut() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "user" {
            continue;
        }
        let Some(mut content) = msg.get_mut("content").and_then(|c| c.as_str().map(String::from))
        else {
            continue;
        };
        if !content.contains(TOOL_RESULTS_HEADER) {
            continue;
        }
        if compress_one(&mut content, live_compress, &excluded) {
            msg["content"] = Value::String(content);
            modified = true;
        }
    }
    modified
}

/// Compress a single user message containing nullclaw XML tool results.
fn compress_one(
    content: &mut String,
    live_compress: bool,
    excluded: &dyn Fn(&str) -> bool,
) -> bool {
    // Split into blocks: header, tool_result blocks, and trailing reflection.
    let parts = split_blocks(content);
    let mut changed = false;
    let mut rebuilt = String::with_capacity(content.len());

    for part in &parts {
        match part {
            Block::Header => rebuilt.push_str(TOOL_RESULTS_HEADER),
            Block::ToolResult {
                name,
                status,
                body,
            } => {
                if !live_compress || excluded(name) || tool_name_is_lean_ctx(name) {
                    // ctx_* tools already compressed at MCP boundary (#479) —
                    // pass through.  Also skip when live compress is off or the
                    // tool is in the exclusion list.
                    rebuilt.push_str(&render_block(name, status, body));
                } else {
                    let kind = tool_kind::classify_tool_name(name);
                    let mut text = body.clone();
                    if super::tool_output::compress_text(&mut text, Some(name.as_str()), kind) {
                        changed = true;
                        rebuilt.push_str(&render_block(name, status, &text));
                    } else {
                        rebuilt.push_str(&render_block(name, status, body));
                    }
                }
            }
            Block::Prose(text) => rebuilt.push_str(text),
        }
    }

    if changed {
        *content = rebuilt;
    }
    changed
}

/// A parsed piece of the user message.
enum Block {
    Header,
    ToolResult {
        name: String,
        status: String,
        body: String,
    },
    /// Anything else (whitespace between blocks, reflection prompt, etc.).
    Prose(String),
}

/// Split the raw message content into structured blocks.
fn split_blocks(content: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut cursor = 0usize;

    // Header
    if let Some(hdr_end) = content.find(TOOL_RESULTS_HEADER) {
        if hdr_end > cursor {
            blocks.push(Block::Prose(content[cursor..hdr_end].to_string()));
        }
        blocks.push(Block::Header);
        cursor = hdr_end + TOOL_RESULTS_HEADER.len();
    }

    // Iterate over <tool_result ...> ... </tool_result> blocks.
    while let Some(open_start) = content[cursor..].find(BLOCK_OPEN) {
        let abs_open = cursor + open_start;

        // Prose before this block.
        if abs_open > cursor {
            let prose = &content[cursor..abs_open];
            if !prose.is_empty() {
                blocks.push(Block::Prose(prose.to_string()));
            }
        }

        // Extract the name attribute.
        let after_open = abs_open + BLOCK_OPEN.len();
        let name_end = match content[after_open..].find('"') {
            Some(pos) => after_open + pos,
            None => break, // malformed — bail
        };
        let name = content[after_open..name_end].to_string();

        // Find the closing '>' of the opening tag.
        let tag_close = match content[name_end..].find('>') {
            Some(pos) => name_end + pos,
            None => break,
        };
        let status = extract_status(&content[abs_open..tag_close]);

        // Find </tool_result>.
        let body_start = tag_close + 1;
        let body_end = match content[body_start..].find(BLOCK_CLOSE) {
            Some(pos) => body_start + pos,
            None => break,
        };

        let body = content[body_start..body_end].trim_matches('\n').to_string();
        blocks.push(Block::ToolResult {
            name,
            status,
            body,
        });

        cursor = body_end + BLOCK_CLOSE.len();
    }

    // Trailing prose (reflection prompt etc.).
    if cursor < content.len() {
        let rest = &content[cursor..];
        if !rest.is_empty() {
            blocks.push(Block::Prose(rest.to_string()));
        }
    }

    blocks
}

/// Extract the `status="..."` attribute from an opening tag.
fn extract_status(tag: &str) -> String {
    if let Some(start) = tag.find("status=\"") {
        let after = start + "status=\"".len();
        if let Some(end) = tag[after..].find('"') {
            return tag[after..after + end].to_string();
        }
    }
    String::from("ok")
}

/// Re-serialise a tool-result block.
fn render_block(name: &str, status: &str, body: &str) -> String {
    format!("\n<tool_result name=\"{name}\" status=\"{status}\">\n{body}\n</tool_result>\n")
}

/// Inline copy of compress.rs `is_lean_ctx_tool` to avoid changing its
/// visibility.  Returns true for any tool name starting with `ctx_`.
fn tool_name_is_lean_ctx(name: &str) -> bool {
    let bare = name
        .rsplit("__")
        .next()
        .unwrap_or(name)
        .rsplit([':', '/', '.'])
        .next()
        .unwrap_or(name);
    bare.starts_with("ctx_") || name.starts_with("ctx_")
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_header_and_single_block() {
        let input = "[Tool results]\n<tool_result name=\"web_fetch\" status=\"ok\">\nhello world\n</tool_result>\n";
        let blocks = split_blocks(input);
        assert!(matches!(blocks[0], Block::Header));
        if let Block::ToolResult {
            name,
            status,
            body,
        } = &blocks[1]
        {
            assert_eq!(name, "web_fetch");
            assert_eq!(status, "ok");
            assert_eq!(body, "hello world");
        } else {
            panic!("expected ToolResult");
        }
    }

    #[test]
    fn preserves_trailing_prose() {
        let input = "[Tool results]\n<tool_result name=\"shell\" status=\"ok\">\nout\n</tool_result>\nReflect on the tool results above.";
        let blocks = split_blocks(input);
        let last = blocks.last().unwrap();
        if let Block::Prose(text) = last {
            assert!(text.contains("Reflect"));
        } else {
            panic!("expected Prose");
        }
    }

    #[test]
    fn ctx_tools_pass_through() {
        assert!(tool_name_is_lean_ctx("ctx_shell"));
        assert!(tool_name_is_lean_ctx("mcp__lean-ctx__ctx_read"));
        assert!(!tool_name_is_lean_ctx("web_fetch"));
        assert!(!tool_name_is_lean_ctx("shell"));
    }

    #[test]
    fn handles_multiple_blocks() {
        let input = "[Tool results]\n<tool_result name=\"shell\" status=\"ok\">\nout1\n</tool_result>\n<tool_result name=\"web_search\" status=\"ok\">\nout2\n</tool_result>\n";
        let blocks = split_blocks(input);
        let tool_blocks: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::ToolResult { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(tool_blocks, vec!["shell", "web_search"]);
    }
}
