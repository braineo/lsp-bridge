//! Completion item resolve — completionItem/resolve (LSP 3.17 §3.18.2).

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::{Handler, RequestContext, ResponseContext};

pub struct CompletionItem;

#[async_trait]
impl Handler for CompletionItem {
    fn name(&self) -> &'static str { "completion_item_resolve" }
    fn method(&self) -> &'static str { "completionItem/resolve" }
    fn send_document_uri(&self) -> bool { false }

    /// Args: [item_key, server_name, item]
    /// The `item` is the original completion item to resolve.
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        // The third arg is the actual completion item to send to the server
        ctx.args.get(2).cloned().ok_or_else(|| anyhow::anyhow!("missing completion item"))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() { return Ok(()); }

        let mut doc = String::new();
        if let Some(documentation) = response.get("documentation") {
            if let Some(s) = documentation.as_str() {
                doc = s.to_string();
            } else if let Some(obj) = documentation.as_object() {
                doc = obj.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string();
            }
        }

        let additional_text_edits = response.get("additionalTextEdits").cloned().unwrap_or(json!([]));

        (ctx.eval_in_emacs)("lsp-bridge-completion-item--update", vec![
            Value::String(doc),
            additional_text_edits,
            Value::String(ctx.server_name.clone()),
        ]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request() {
        let item = json!({"label": "print", "kind": 3});
        let ctx = RequestContext { args: vec![json!("print_"), json!("pyright"), item.clone()], server_name: "pyright".into(), trigger_characters: vec![], server_info: json!({}) };
        let params = CompletionItem.process_request(&ctx).unwrap();
        assert_eq!(params["label"], "print");
    }
    #[test]
    fn properties() {
        assert_eq!(CompletionItem.method(), "completionItem/resolve");
        assert!(!CompletionItem.send_document_uri());
        assert!(!CompletionItem.cancel_on_change());
    }
}
