//! Document highlight — textDocument/documentHighlight (LSP 3.17 §3.18.16).

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::{Handler, RequestContext, ResponseContext};

pub struct DocumentHighlight;

#[async_trait]
impl Handler for DocumentHighlight {
    fn name(&self) -> &'static str { "document_highlight" }
    fn method(&self) -> &'static str { "textDocument/documentHighlight" }
    fn cancel_on_change(&self) -> bool { true }

    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let position = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        Ok(json!({"position": position}))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() { return Ok(()); }
        (ctx.eval_in_emacs)("lsp-bridge-document-highlight-render", vec![response]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request() {
        let ctx = RequestContext { args: vec![json!({"line": 5, "character": 3})], server_name: "pyright".into(), trigger_characters: vec![], server_info: json!({}) };
        let params = DocumentHighlight.process_request(&ctx).unwrap();
        assert_eq!(params["position"]["line"], 5);
    }
    #[test]
    fn properties() {
        assert_eq!(DocumentHighlight.method(), "textDocument/documentHighlight");
        assert!(DocumentHighlight.cancel_on_change());
    }
}
