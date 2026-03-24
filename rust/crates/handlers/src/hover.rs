//! Hover handler — textDocument/hover (LSP 3.17 §3.18.4).
//!
//! Mirrors Python's core/handler/hover.py.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{Handler, RequestContext, ResponseContext};

pub struct Hover;

#[async_trait]
impl Handler for Hover {
    fn name(&self) -> &'static str {
        "hover"
    }

    fn method(&self) -> &'static str {
        "textDocument/hover"
    }

    /// Build hover request params.
    ///
    /// Args from Emacs: [start, end, show_style]
    /// Python: dict(position=start) for normal; dict(position={"start": s, "end": e}) for range
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let start = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        let end = ctx.args.get(1).cloned().unwrap_or(Value::Null);

        // rust-analyzer supports range hover (non-standard extension)
        if start == end || ctx.server_name != "rust-analyzer" {
            Ok(json!({"position": start}))
        } else {
            Ok(json!({"position": {"start": start, "end": end}}))
        }
    }

    /// Process hover response.
    ///
    /// LSP 3.17 §3.18.4: response is Hover | null
    /// Hover = { contents: MarkedString | MarkedString[] | MarkupContent, range?: Range }
    async fn process_response(
        &self,
        ctx: &ResponseContext,
        response: Value,
    ) -> anyhow::Result<()> {
        if response.is_null() {
            (ctx.message_emacs)("No documentation available.");
            return Ok(());
        }

        let contents = match response.get("contents") {
            Some(c) if !is_empty_content(c) => c,
            _ => {
                (ctx.message_emacs)("No documentation available.");
                return Ok(());
            }
        };

        let render_string = parse_hover_contents(contents);

        // show_style determines the Emacs callback
        // Default to popup for now
        let callback = "lsp-bridge-popup-documentation--callback";

        (ctx.eval_in_emacs)(callback, vec![Value::String(render_string)]);

        Ok(())
    }
}

fn is_empty_content(contents: &Value) -> bool {
    match contents {
        Value::String(s) => s.is_empty(),
        Value::Array(arr) => arr.is_empty(),
        Value::Object(obj) => {
            obj.get("value")
                .and_then(|v| v.as_str())
                .map(|s| s.is_empty())
                .unwrap_or(true)
        }
        _ => true,
    }
}

/// Parse hover contents into a markdown string.
///
/// Mirrors Python's parse_hover_contents. Handles:
/// - String → wrap in code block if not already markdown
/// - MarkupContent { kind, value } → use value directly
/// - MarkedString { language, value } → wrap in code block
/// - Array of mixed content → join with newlines
fn parse_hover_contents(contents: &Value) -> String {
    let mut parts = Vec::new();
    parse_hover_contents_inner(contents, &mut parts);
    parts.join("\n")
}

fn parse_hover_contents_inner(contents: &Value, parts: &mut Vec<String>) {
    match contents {
        Value::String(s) => {
            if s.starts_with("```") {
                parts.push(s.clone());
            } else {
                parts.push(make_code_block("text", s));
            }
        }
        Value::Object(obj) => {
            if let Some(kind) = obj.get("kind").and_then(|k| k.as_str()) {
                // MarkupContent: { kind: "markdown"|"plaintext", value: string }
                let value = obj.get("value").and_then(|v| v.as_str()).unwrap_or("");
                if kind == "markdown" || kind == "plaintext" {
                    parts.push(value.to_string());
                } else {
                    parts.push(make_code_block(kind, value));
                }
            } else if let Some(language) = obj.get("language").and_then(|l| l.as_str()) {
                // MarkedString: { language: string, value: string }
                let value = obj.get("value").and_then(|v| v.as_str()).unwrap_or("");
                parts.push(make_code_block(language, value));
            }
        }
        Value::Array(arr) => {
            for item in arr {
                parse_hover_contents_inner(item, parts);
            }
        }
        _ => {}
    }
}

fn make_code_block(language: &str, content: &str) -> String {
    format!("```{}\n{}\n```", language, content)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_ctx(args: Vec<Value>, server: &str) -> RequestContext {
        RequestContext {
            args,
            server_name: server.to_string(),
            trigger_characters: vec![],
            server_info: json!({"name": server}),
        }
    }

    // Python ground truth: dict(position=start) when start==end
    #[test]
    fn request_point_position() {
        let handler = Hover;
        let pos = json!({"line": 5, "character": 3});
        let ctx = make_ctx(vec![pos.clone(), pos.clone(), json!("popup")], "pyright");
        let params = handler.process_request(&ctx).unwrap();
        assert_eq!(params, json!({"position": {"line": 5, "character": 3}}));
    }

    // Python ground truth: range hover for rust-analyzer
    #[test]
    fn request_range_rust_analyzer() {
        let handler = Hover;
        let start = json!({"line": 5, "character": 0});
        let end = json!({"line": 5, "character": 10});
        let ctx = make_ctx(
            vec![start.clone(), end.clone(), json!("popup")],
            "rust-analyzer",
        );
        let params = handler.process_request(&ctx).unwrap();
        assert_eq!(
            params,
            json!({"position": {"start": {"line": 5, "character": 0}, "end": {"line": 5, "character": 10}}})
        );
    }

    // Non-rust-analyzer with different start/end → use start only
    #[test]
    fn request_range_non_rust_analyzer() {
        let handler = Hover;
        let start = json!({"line": 5, "character": 0});
        let end = json!({"line": 5, "character": 10});
        let ctx = make_ctx(
            vec![start.clone(), end.clone(), json!("popup")],
            "pyright",
        );
        let params = handler.process_request(&ctx).unwrap();
        assert_eq!(params, json!({"position": {"line": 5, "character": 0}}));
    }

    #[test]
    fn parse_markdown_content() {
        let contents = json!({"kind": "markdown", "value": "# Hello\nWorld"});
        assert_eq!(parse_hover_contents(&contents), "# Hello\nWorld");
    }

    #[test]
    fn parse_plaintext_content() {
        let contents = json!({"kind": "plaintext", "value": "Hello World"});
        assert_eq!(parse_hover_contents(&contents), "Hello World");
    }

    #[test]
    fn parse_markedstring_with_language() {
        let contents = json!({"language": "python", "value": "def foo(): ..."});
        assert_eq!(
            parse_hover_contents(&contents),
            "```python\ndef foo(): ...\n```"
        );
    }

    #[test]
    fn parse_plain_string() {
        let contents = json!("Hello World");
        assert_eq!(parse_hover_contents(&contents), "```text\nHello World\n```");
    }

    #[test]
    fn parse_markdown_string() {
        let contents = json!("```python\ndef foo(): ...\n```");
        assert_eq!(
            parse_hover_contents(&contents),
            "```python\ndef foo(): ...\n```"
        );
    }

    #[test]
    fn parse_array_contents() {
        let contents = json!([
            {"language": "python", "value": "def foo(): ..."},
            "Documentation text"
        ]);
        let result = parse_hover_contents(&contents);
        assert!(result.contains("```python"));
        assert!(result.contains("```text\nDocumentation text\n```"));
    }

    #[test]
    fn handler_properties() {
        let handler = Hover;
        assert_eq!(handler.name(), "hover");
        assert_eq!(handler.method(), "textDocument/hover");
        assert!(!handler.cancel_on_change());
    }
}
