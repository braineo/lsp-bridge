//! Completion handler — textDocument/completion (LSP 3.17 §3.18.1).
//!
//! Mirrors Python's core/handler/completion.py.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{kind_name, Handler, RequestContext, ResponseContext};

/// LSP CompletionTriggerKind per LSP 3.17 spec.
pub const TRIGGER_KIND_INVOKED: u32 = 1;
pub const TRIGGER_KIND_TRIGGER_CHARACTER: u32 = 2;
pub const TRIGGER_KIND_INCOMPLETE: u32 = 3;

pub struct Completion;

#[async_trait]
impl Handler for Completion {
    fn name(&self) -> &'static str {
        "completion"
    }

    fn method(&self) -> &'static str {
        "textDocument/completion"
    }

    fn cancel_on_change(&self) -> bool {
        true
    }

    /// Build completion request params.
    ///
    /// Args from Emacs: [position, char, prefix, version]
    /// Python ground truth:
    ///   trigger char: {"position": pos, "context": {"triggerCharacter": ".", "triggerKind": 2}}
    ///   invoked:      {"position": pos, "context": {"triggerKind": 1}}
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let position = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        let char = ctx.args.get(1).and_then(|v| v.as_str()).unwrap_or("");

        let context = if ctx.trigger_characters.iter().any(|tc| tc == char) {
            json!({
                "triggerCharacter": char,
                "triggerKind": TRIGGER_KIND_TRIGGER_CHARACTER
            })
        } else {
            json!({
                "triggerKind": TRIGGER_KIND_INVOKED
            })
        };

        Ok(json!({
            "position": position,
            "context": context
        }))
    }

    /// Process completion response and send candidates to Emacs.
    ///
    /// LSP 3.17 §3.18.1: response is CompletionList or CompletionItem[].
    async fn process_response(
        &self,
        ctx: &ResponseContext,
        response: Value,
    ) -> anyhow::Result<()> {
        let items = if let Some(items) = response.get("items") {
            items.as_array().cloned().unwrap_or_default()
        } else if response.is_array() {
            response.as_array().cloned().unwrap_or_default()
        } else {
            vec![]
        };

        let mut candidates: Vec<Value> = Vec::new();

        for item in &items {
            let kind_num = item.get("kind").and_then(|k| k.as_u64()).unwrap_or(0);
            let kind = kind_name(kind_num).to_lowercase();
            let label = item.get("label").and_then(|l| l.as_str()).unwrap_or("");
            let detail = item.get("detail").and_then(|d| d.as_str()).unwrap_or("");

            let annotation = if !kind.is_empty() { &kind } else { detail };

            let key = format!("{}_{}", label, detail);

            let insert_text = item.get("insertText").cloned();
            let text_edit = item.get("textEdit").cloned();

            let candidate = json!({
                "key": key,
                "icon": annotation,
                "label": label,
                "displayLabel": label,
                "deprecated": item.get("tags").and_then(|t| t.as_array())
                    .map(|tags| tags.iter().any(|t| t.as_u64() == Some(1)))
                    .unwrap_or(false),
                "insertText": insert_text,
                "insertTextFormat": item.get("insertTextFormat").cloned().unwrap_or(Value::String(String::new())),
                "textEdit": text_edit,
                "score": item.get("score").and_then(|s| s.as_f64()).unwrap_or(1000.0),
                "sortText": item.get("sortText").and_then(|s| s.as_str()).unwrap_or(""),
                "filterText": item.get("filterText").cloned(),
                "server": &ctx.server_name,
                "backend": "lsp"
            });

            candidates.push(candidate);
        }

        // Call lsp-bridge-completion--record-items in Emacs
        (ctx.eval_in_emacs)(
            "lsp-bridge-completion--record-items",
            vec![
                Value::String(ctx.filepath.clone()),
                Value::String(ctx.host.clone()),
                Value::Array(candidates),
                // position — passed as first arg in process_request
                Value::Null, // TODO: pass position from request context
                Value::String(ctx.server_name.clone()),
                Value::Array(
                    ctx.trigger_characters
                        .iter()
                        .map(|s| Value::String(s.clone()))
                        .collect(),
                ),
                Value::Array(
                    ctx.server_names
                        .iter()
                        .map(|s| Value::String(s.clone()))
                        .collect(),
                ),
            ],
        );

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_ctx(args: Vec<Value>, trigger_chars: Vec<&str>) -> RequestContext {
        RequestContext {
            args,
            server_name: "pyright".to_string(),
            trigger_characters: trigger_chars.into_iter().map(|s| s.to_string()).collect(),
            server_info: json!({"name": "pyright"}),
        }
    }

    // Python ground truth:
    // char in trigger_characters → {"position": pos, "context": {"triggerCharacter": ".", "triggerKind": 2}}
    #[test]
    fn request_trigger_character() {
        let handler = Completion;
        let ctx = make_ctx(
            vec![json!({"line": 10, "character": 5}), json!(".")],
            vec![".", "["],
        );
        let params = handler.process_request(&ctx).unwrap();
        assert_eq!(params["position"]["line"], 10);
        assert_eq!(params["position"]["character"], 5);
        assert_eq!(params["context"]["triggerKind"], 2);
        assert_eq!(params["context"]["triggerCharacter"], ".");
    }

    // Python ground truth:
    // char NOT in trigger_characters → {"position": pos, "context": {"triggerKind": 1}}
    #[test]
    fn request_invoked() {
        let handler = Completion;
        let ctx = make_ctx(
            vec![json!({"line": 10, "character": 5}), json!("a")],
            vec![".", "["],
        );
        let params = handler.process_request(&ctx).unwrap();
        assert_eq!(params["context"]["triggerKind"], 1);
        assert!(params["context"].get("triggerCharacter").is_none());
    }

    #[test]
    fn request_no_trigger_chars() {
        let handler = Completion;
        let ctx = make_ctx(
            vec![json!({"line": 0, "character": 0}), json!(".")],
            vec![],
        );
        let params = handler.process_request(&ctx).unwrap();
        assert_eq!(params["context"]["triggerKind"], 1); // no trigger chars → invoked
    }

    #[test]
    fn handler_properties() {
        let handler = Completion;
        assert_eq!(handler.name(), "completion");
        assert_eq!(handler.method(), "textDocument/completion");
        assert!(handler.cancel_on_change());
        assert!(handler.send_document_uri());
    }
}
