//! Inlay hints — textDocument/inlayHint (LSP 3.17 §3.18.14).

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::{Handler, RequestContext, ResponseContext};

pub struct InlayHint;

#[async_trait]
impl Handler for InlayHint {
    fn name(&self) -> &'static str { "inlay_hint" }
    fn method(&self) -> &'static str { "textDocument/inlayHint" }
    fn cancel_on_change(&self) -> bool { true }

    /// Args: [range_start, range_end]
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let start = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        let end = ctx.args.get(1).cloned().unwrap_or(Value::Null);
        Ok(json!({"range": {"start": start, "end": end}}))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() { return Ok(()); }
        if let Some(hints) = response.as_array() {
            (ctx.eval_in_emacs)("lsp-bridge-inlay-hint--render", vec![
                Value::String(ctx.filepath.clone()),
                Value::String(ctx.host.clone()),
                Value::Array(hints.clone()),
            ]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request() {
        let ctx = RequestContext { args: vec![json!({"line": 0, "character": 0}), json!({"line": 50, "character": 0})], server_name: "rust-analyzer".into(), trigger_characters: vec![], server_info: json!({}) };
        let params = InlayHint.process_request(&ctx).unwrap();
        assert_eq!(params["range"]["start"]["line"], 0);
        assert_eq!(params["range"]["end"]["line"], 50);
    }

    #[test]
    fn properties() {
        assert_eq!(InlayHint.method(), "textDocument/inlayHint");
        assert!(InlayHint.cancel_on_change());
    }
}
