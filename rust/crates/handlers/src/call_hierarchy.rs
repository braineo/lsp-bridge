//! Call hierarchy — textDocument/prepareCallHierarchy + callHierarchy/incomingCalls/outgoingCalls
//! (LSP 3.17 §3.18.19).

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::{Handler, RequestContext, ResponseContext};

pub struct PrepareCallHierarchyIncoming;

#[async_trait]
impl Handler for PrepareCallHierarchyIncoming {
    fn name(&self) -> &'static str { "prepare_call_hierarchy_incoming" }
    fn method(&self) -> &'static str { "textDocument/prepareCallHierarchy" }

    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let position = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        Ok(json!({"position": position}))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() {
            (ctx.message_emacs)("No call hierarchies found");
            return Ok(());
        }
        if let Some(arr) = response.as_array() {
            if let Some(first) = arr.first() {
                (ctx.eval_in_emacs)("lsp-bridge-call-hierarchy--incoming", vec![first.clone()]);
            }
        }
        Ok(())
    }
}

pub struct PrepareCallHierarchyOutgoing;

#[async_trait]
impl Handler for PrepareCallHierarchyOutgoing {
    fn name(&self) -> &'static str { "prepare_call_hierarchy_outgoing" }
    fn method(&self) -> &'static str { "textDocument/prepareCallHierarchy" }

    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let position = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        Ok(json!({"position": position}))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() {
            (ctx.message_emacs)("No call hierarchies found");
            return Ok(());
        }
        if let Some(arr) = response.as_array() {
            if let Some(first) = arr.first() {
                (ctx.eval_in_emacs)("lsp-bridge-call-hierarchy--outgoing", vec![first.clone()]);
            }
        }
        Ok(())
    }
}

pub struct CallHierarchyIncoming;

#[async_trait]
impl Handler for CallHierarchyIncoming {
    fn name(&self) -> &'static str { "call_hierarchy_incoming" }
    fn method(&self) -> &'static str { "callHierarchy/incomingCalls" }
    fn send_document_uri(&self) -> bool { false }

    /// Args: [item]
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let item = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        Ok(json!({"item": item}))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() { return Ok(()); }
        (ctx.eval_in_emacs)("lsp-bridge-call-hierarchy--render", vec![
            json!("incoming"), response
        ]);
        Ok(())
    }
}

pub struct CallHierarchyOutgoing;

#[async_trait]
impl Handler for CallHierarchyOutgoing {
    fn name(&self) -> &'static str { "call_hierarchy_outgoing" }
    fn method(&self) -> &'static str { "callHierarchy/outgoingCalls" }
    fn send_document_uri(&self) -> bool { false }

    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let item = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        Ok(json!({"item": item}))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() { return Ok(()); }
        (ctx.eval_in_emacs)("lsp-bridge-call-hierarchy--render", vec![
            json!("outgoing"), response
        ]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepare_incoming_request() {
        let ctx = RequestContext { args: vec![json!({"line": 5, "character": 3})], server_name: "pyright".into(), trigger_characters: vec![], server_info: json!({}) };
        let params = PrepareCallHierarchyIncoming.process_request(&ctx).unwrap();
        assert_eq!(params["position"]["line"], 5);
    }
    #[test]
    fn incoming_calls_request() {
        let item = json!({"name": "foo", "kind": 12, "uri": "file:///t.py"});
        let ctx = RequestContext { args: vec![item.clone()], server_name: "pyright".into(), trigger_characters: vec![], server_info: json!({}) };
        let params = CallHierarchyIncoming.process_request(&ctx).unwrap();
        assert_eq!(params["item"]["name"], "foo");
    }
    #[test]
    fn methods_per_lsp_spec() {
        assert_eq!(PrepareCallHierarchyIncoming.method(), "textDocument/prepareCallHierarchy");
        assert_eq!(CallHierarchyIncoming.method(), "callHierarchy/incomingCalls");
        assert_eq!(CallHierarchyOutgoing.method(), "callHierarchy/outgoingCalls");
    }
}
