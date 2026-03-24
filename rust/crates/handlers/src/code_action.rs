//! Code action handler — textDocument/codeAction (LSP 3.17 §3.18.11).
//!
//! Mirrors Python's core/handler/code_action.py.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{Handler, RequestContext, ResponseContext};

pub struct CodeAction;

#[async_trait]
impl Handler for CodeAction {
    fn name(&self) -> &'static str {
        "code_action"
    }

    fn method(&self) -> &'static str {
        "textDocument/codeAction"
    }

    fn cancel_on_change(&self) -> bool {
        true
    }

    /// Build code action request params.
    ///
    /// Args from Emacs: [diagnostics, range_start, range_end, action_kind]
    /// Python ground truth:
    ///   {"range": {"start": s, "end": e}, "context": {"diagnostics": [...], "only": [kind]}}
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let diagnostics = ctx.args.get(0).cloned().unwrap_or(json!([]));
        let range_start = ctx.args.get(1).cloned().unwrap_or(Value::Null);
        let range_end = ctx.args.get(2).cloned().unwrap_or(Value::Null);
        let action_kind = ctx.args.get(3).cloned().unwrap_or(Value::Null);

        let mut context = json!({
            "diagnostics": diagnostics
        });

        // Per LSP 3.17: CodeActionContext.only is optional filter
        if let Some(kind_str) = action_kind.as_str() {
            context["only"] = json!([kind_str]);
        }

        Ok(json!({
            "range": {
                "start": range_start,
                "end": range_end
            },
            "context": context
        }))
    }

    /// Process code action response.
    ///
    /// LSP 3.17 §3.18.11: response is (Command | CodeAction)[] | null
    async fn process_response(
        &self,
        ctx: &ResponseContext,
        response: Value,
    ) -> anyhow::Result<()> {
        if response.is_null() {
            return Ok(());
        }

        let actions = response.as_array().cloned().unwrap_or_default();

        if actions.is_empty() {
            return Ok(());
        }

        (ctx.eval_in_emacs)(
            "lsp-bridge-code-action--fix",
            vec![Value::Array(actions), Value::Null],
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

    fn make_ctx(args: Vec<Value>) -> RequestContext {
        RequestContext {
            args,
            server_name: "pyright".to_string(),
            trigger_characters: vec![],
            server_info: json!({"name": "pyright"}),
        }
    }

    // Python ground truth: {"range": {"start": s, "end": e}, "context": {"diagnostics": [...]}}
    #[test]
    fn request_with_diagnostics() {
        let handler = CodeAction;
        let diags = json!([{"message": "unused import", "severity": 2}]);
        let start = json!({"line": 1, "character": 0});
        let end = json!({"line": 1, "character": 10});
        let ctx = make_ctx(vec![diags, start, end, Value::Null]);
        let params = handler.process_request(&ctx).unwrap();

        assert_eq!(params["range"]["start"]["line"], 1);
        assert_eq!(params["range"]["end"]["character"], 10);
        assert_eq!(params["context"]["diagnostics"][0]["message"], "unused import");
    }

    // With action_kind filter
    #[test]
    fn request_with_action_kind() {
        let handler = CodeAction;
        let ctx = make_ctx(vec![
            json!([]),
            json!({"line": 0, "character": 0}),
            json!({"line": 0, "character": 5}),
            json!("quickfix"),
        ]);
        let params = handler.process_request(&ctx).unwrap();
        assert_eq!(params["context"]["only"], json!(["quickfix"]));
    }

    #[test]
    fn request_without_action_kind() {
        let handler = CodeAction;
        let ctx = make_ctx(vec![
            json!([]),
            json!({"line": 0, "character": 0}),
            json!({"line": 0, "character": 5}),
            Value::Null,
        ]);
        let params = handler.process_request(&ctx).unwrap();
        assert!(params["context"].get("only").is_none());
    }

    #[test]
    fn handler_properties() {
        let handler = CodeAction;
        assert_eq!(handler.name(), "code_action");
        assert_eq!(handler.method(), "textDocument/codeAction");
        assert!(handler.cancel_on_change());
    }
}
