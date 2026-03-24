//! Workspace symbol — workspace/symbol (LSP 3.17 §3.18.18).

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::{Handler, RequestContext, ResponseContext};

pub struct WorkspaceSymbol;

#[async_trait]
impl Handler for WorkspaceSymbol {
    fn name(&self) -> &'static str { "workspace_symbol" }
    fn method(&self) -> &'static str { "workspace/symbol" }

    /// Args: [query]
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let query = ctx.args.get(0).and_then(|v| v.as_str()).unwrap_or("");
        // Python: ''.join(query.split()) — remove all whitespace
        let query: String = query.split_whitespace().collect();
        Ok(json!({"query": query}))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() { return Ok(()); }
        (ctx.eval_in_emacs)("lsp-bridge-workspace--list-symbols", vec![response]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_strips_whitespace() {
        let ctx = RequestContext { args: vec![json!("my func")], server_name: "pyright".into(), trigger_characters: vec![], server_info: json!({}) };
        let params = WorkspaceSymbol.process_request(&ctx).unwrap();
        assert_eq!(params["query"], "myfunc");
    }
    #[test]
    fn properties() {
        assert_eq!(WorkspaceSymbol.method(), "workspace/symbol");
    }
}
