//! Semantic tokens — textDocument/semanticTokens/full (LSP 3.17 §3.18.13).

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::{Handler, RequestContext, ResponseContext};

pub struct SemanticTokens;

#[async_trait]
impl Handler for SemanticTokens {
    fn name(&self) -> &'static str { "semantic_tokens" }
    fn method(&self) -> &'static str { "textDocument/semanticTokens/full" }

    fn process_request(&self, _ctx: &RequestContext) -> anyhow::Result<Value> {
        Ok(json!({})) // Only needs textDocument URI (auto-added)
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() { return Ok(()); }
        (ctx.eval_in_emacs)("lsp-bridge-semantic-tokens--render", vec![
            Value::String(ctx.filepath.clone()),
            Value::String(ctx.host.clone()),
            response,
        ]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn properties() {
        assert_eq!(SemanticTokens.method(), "textDocument/semanticTokens/full");
        assert!(!SemanticTokens.cancel_on_change());
    }
}
