//! Pull-based diagnostics — textDocument/diagnostic (LSP 3.17 §3.18.15).

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::{Handler, RequestContext, ResponseContext};

pub struct Diagnostic;

#[async_trait]
impl Handler for Diagnostic {
    fn name(&self) -> &'static str { "diagnostic" }
    fn method(&self) -> &'static str { "textDocument/diagnostic" }

    /// Args: [identifier?, previous_result_id?]
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let mut params = json!({});
        if let Some(id) = ctx.args.get(0).and_then(|v| v.as_str()) {
            params["identifier"] = json!(id);
        }
        if let Some(prev) = ctx.args.get(1).and_then(|v| v.as_str()) {
            params["previousResultId"] = json!(prev);
        }
        Ok(params)
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() { return Ok(()); }
        if let Some(items) = response.get("items").and_then(|i| i.as_array()) {
            (ctx.eval_in_emacs)("lsp-bridge-diagnostic--render", vec![
                Value::String(ctx.filepath.clone()),
                Value::String(ctx.host.clone()),
                Value::Array(items.clone()),
                json!(items.len()),
            ]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_empty() {
        let ctx = RequestContext { args: vec![], server_name: "pyright".into(), trigger_characters: vec![], server_info: json!({}) };
        let params = Diagnostic.process_request(&ctx).unwrap();
        assert!(params.as_object().unwrap().is_empty());
    }
    #[test]
    fn properties() { assert_eq!(Diagnostic.method(), "textDocument/diagnostic"); }
}
