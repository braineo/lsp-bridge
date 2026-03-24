//! Formatting handlers — textDocument/formatting, rangeFormatting (LSP 3.17 §3.18.10).

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::{Handler, RequestContext, ResponseContext};

pub struct Formatting;

#[async_trait]
impl Handler for Formatting {
    fn name(&self) -> &'static str { "formatting" }
    fn method(&self) -> &'static str { "textDocument/formatting" }
    fn cancel_on_change(&self) -> bool { true }

    /// Args: [tab_size]
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let tab_size = ctx.args.get(0).cloned().unwrap_or(json!(4));
        Ok(json!({
            "options": {
                "tabSize": tab_size,
                "insertSpaces": true,
                "trimTrailingWhitespace": true,
                "insertFinalNewline": false,
                "trimFinalNewlines": true
            }
        }))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() || response.as_array().is_some_and(|a| a.is_empty()) {
            (ctx.message_emacs)("Nothing need format.");
        } else {
            (ctx.eval_in_emacs)("lsp-bridge-format--update", vec![
                Value::String(ctx.filepath.clone()), response
            ]);
        }
        Ok(())
    }
}

pub struct RangeFormatting;

#[async_trait]
impl Handler for RangeFormatting {
    fn name(&self) -> &'static str { "rangeFormatting" }
    fn method(&self) -> &'static str { "textDocument/rangeFormatting" }
    fn cancel_on_change(&self) -> bool { true }

    /// Args: [range_start, range_end, tab_size]
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let range_start = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        let range_end = ctx.args.get(1).cloned().unwrap_or(Value::Null);
        let tab_size = ctx.args.get(2).cloned().unwrap_or(json!(4));
        Ok(json!({
            "range": {"start": range_start, "end": range_end},
            "options": {
                "tabSize": tab_size,
                "insertSpaces": true,
                "trimTrailingWhitespace": true,
                "insertFinalNewline": false,
                "trimFinalNewlines": true
            }
        }))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() || response.as_array().is_some_and(|a| a.is_empty()) {
            (ctx.message_emacs)("Nothing need format.");
        } else {
            (ctx.eval_in_emacs)("lsp-bridge-format--update", vec![
                Value::String(ctx.filepath.clone()), response
            ]);
        }
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
    fn formatting_request() {
        let params = Formatting.process_request(&make_ctx(vec![json!(4)])).unwrap();
        assert_eq!(params["options"]["tabSize"], 4);
        assert_eq!(params["options"]["trimTrailingWhitespace"], true);
    }

    #[test]
    fn range_formatting_request() {
        let params = RangeFormatting.process_request(&make_ctx(vec![
            json!({"line": 0, "character": 0}),
            json!({"line": 10, "character": 0}),
            json!(2),
        ])).unwrap();
        assert_eq!(params["range"]["start"]["line"], 0);
        assert_eq!(params["range"]["end"]["line"], 10);
        assert_eq!(params["options"]["tabSize"], 2);
    }
}
