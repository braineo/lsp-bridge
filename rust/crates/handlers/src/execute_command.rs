//! Execute command — workspace/executeCommand (LSP 3.17 §3.18.17).

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::{Handler, RequestContext, ResponseContext};

pub struct ExecuteCommand;

#[async_trait]
impl Handler for ExecuteCommand {
    fn name(&self) -> &'static str { "execute_command" }
    fn method(&self) -> &'static str { "workspace/executeCommand" }
    fn send_document_uri(&self) -> bool { false }

    /// Args: [server_name, command, arguments]
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let command = ctx.args.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let arguments = ctx.args.get(2).cloned().unwrap_or(json!([]));
        Ok(json!({"command": command, "arguments": arguments}))
    }

    async fn process_response(&self, _ctx: &ResponseContext, _response: Value) -> anyhow::Result<()> {
        Ok(()) // Execute command typically has no meaningful response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request() {
        let ctx = RequestContext { args: vec![json!("pyright"), json!("organizeImports"), json!([{"uri": "file:///tmp/t.py"}])], server_name: "pyright".into(), trigger_characters: vec![], server_info: json!({}) };
        let params = ExecuteCommand.process_request(&ctx).unwrap();
        assert_eq!(params["command"], "organizeImports");
        assert!(params["arguments"].is_array());
    }
    #[test]
    fn properties() {
        assert_eq!(ExecuteCommand.method(), "workspace/executeCommand");
        assert!(!ExecuteCommand.send_document_uri());
    }
}
