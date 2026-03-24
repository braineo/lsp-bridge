//! Rename handlers — textDocument/prepareRename, textDocument/rename (LSP 3.17 §3.18.9).

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::{Handler, RequestContext, ResponseContext};

pub struct PrepareRename;

#[async_trait]
impl Handler for PrepareRename {
    fn name(&self) -> &'static str { "prepare_rename" }
    fn method(&self) -> &'static str { "textDocument/prepareRename" }

    /// Args: [position]
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let position = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        Ok(json!({"position": position}))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() { return Ok(()); }
        // Compatible with gopls: may have "range" wrapper
        let range = if response.get("range").is_some() {
            &response["range"]
        } else {
            &response
        };
        (ctx.eval_in_emacs)("lsp-bridge-rename--highlight", vec![
            Value::String(ctx.filepath.clone()),
            Value::String(ctx.host.clone()),
            range.get("start").cloned().unwrap_or(Value::Null),
            range.get("end").cloned().unwrap_or(Value::Null),
        ]);
        Ok(())
    }
}

pub struct Rename;

#[async_trait]
impl Handler for Rename {
    fn name(&self) -> &'static str { "rename" }
    fn method(&self) -> &'static str { "textDocument/rename" }

    /// Args: [position, new_name]
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let position = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        let new_name = ctx.args.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
        Ok(json!({"position": position, "newName": new_name}))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() {
            (ctx.message_emacs)("No rename found");
            return Ok(());
        }
        (ctx.eval_in_emacs)("lsp-bridge-workspace-apply-edit", vec![response]);
        (ctx.message_emacs)("Rename done.");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ctx(args: Vec<Value>) -> RequestContext {
        RequestContext { args, server_name: "pyright".into(), trigger_characters: vec![], server_info: json!({}) }
    }

    #[test]
    fn rename_request() {
        let params = Rename.process_request(&make_ctx(vec![
            json!({"line": 5, "character": 10}), json!("new_name")
        ])).unwrap();
        assert_eq!(params["position"]["line"], 5);
        assert_eq!(params["newName"], "new_name");
    }

    #[test]
    fn prepare_rename_request() {
        let params = PrepareRename.process_request(&make_ctx(vec![
            json!({"line": 5, "character": 10})
        ])).unwrap();
        assert_eq!(params["position"]["line"], 5);
    }
}
