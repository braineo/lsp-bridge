//! Signature help — textDocument/signatureHelp (LSP 3.17 §3.18.3).

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::{Handler, RequestContext, ResponseContext};

pub struct SignatureHelp;

#[async_trait]
impl Handler for SignatureHelp {
    fn name(&self) -> &'static str { "signature_help" }
    fn method(&self) -> &'static str { "textDocument/signatureHelp" }
    fn cancel_on_change(&self) -> bool { true }

    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let position = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        Ok(json!({"position": position}))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() { return Ok(()); }
        let signatures = response.get("signatures").and_then(|s| s.as_array());
        if signatures.is_none_or(|s| s.is_empty()) { return Ok(()); }

        let active_sig = response.get("activeSignature").and_then(|s| s.as_u64()).unwrap_or(0) as usize;
        let active_param = response.get("activeParameter").and_then(|p| p.as_u64()).unwrap_or(0) as usize;
        let sigs = signatures.unwrap();
        let sig = sigs.get(active_sig).unwrap_or(&sigs[0]);

        if let Some(params) = sig.get("parameters").and_then(|p| p.as_array()) {
            let arguments: Vec<String> = params.iter().enumerate().map(|(i, param)| {
                let label = &param["label"];
                if let Some(s) = label.as_str() {
                    if i == active_param { s.to_string() } else { s.split(':').next().unwrap_or(s).to_string() }
                } else if let Some(arr) = label.as_array() {
                    if arr.len() == 2 {
                        let start = arr[0].as_u64().unwrap_or(0) as usize;
                        let end = arr[1].as_u64().unwrap_or(0) as usize;
                        sig.get("label").and_then(|l| l.as_str()).map(|l| l[start..end.min(l.len())].to_string()).unwrap_or_default()
                    } else { String::new() }
                } else { String::new() }
            }).collect();

            (ctx.eval_in_emacs)("lsp-bridge-signature-help--update", vec![
                Value::String(arguments.join(", ")),
                json!(active_param),
                Value::String(sig.get("label").and_then(|l| l.as_str()).unwrap_or("").to_string()),
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
        let ctx = RequestContext { args: vec![json!({"line": 5, "character": 3})], server_name: "pyright".into(), trigger_characters: vec![], server_info: json!({}) };
        let params = SignatureHelp.process_request(&ctx).unwrap();
        assert_eq!(params["position"]["line"], 5);
    }

    #[test]
    fn properties() {
        assert_eq!(SignatureHelp.method(), "textDocument/signatureHelp");
        assert!(SignatureHelp.cancel_on_change());
    }
}
