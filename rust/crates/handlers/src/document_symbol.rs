//! Document symbols — textDocument/documentSymbol (LSP 3.17 §3.18.12).

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::{Handler, RequestContext, ResponseContext};

pub struct DocumentSymbol;

#[async_trait]
impl Handler for DocumentSymbol {
    fn name(&self) -> &'static str { "document_symbol" }
    fn method(&self) -> &'static str { "textDocument/documentSymbol" }

    fn process_request(&self, _ctx: &RequestContext) -> anyhow::Result<Value> {
        Ok(json!({})) // Only needs textDocument URI
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() { return Ok(()); }
        (ctx.eval_in_emacs)("lsp-bridge-document-symbol--render", vec![
            Value::String(ctx.filepath.clone()),
            Value::String(ctx.host.clone()),
            response,
        ]);
        Ok(())
    }
}

/// IMenu handler — uses documentSymbol but with different callback.
pub struct IMenu;

#[async_trait]
impl Handler for IMenu {
    fn name(&self) -> &'static str { "imenu" }
    fn method(&self) -> &'static str { "textDocument/documentSymbol" }

    fn process_request(&self, _ctx: &RequestContext) -> anyhow::Result<Value> {
        Ok(json!({}))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() { return Ok(()); }
        (ctx.eval_in_emacs)("lsp-bridge-imenu--render", vec![response]);
        Ok(())
    }
}

/// Breadcrumb handler — also uses documentSymbol.
pub struct Breadcrumb;

#[async_trait]
impl Handler for Breadcrumb {
    fn name(&self) -> &'static str { "breadcrumb" }
    fn method(&self) -> &'static str { "textDocument/documentSymbol" }

    fn process_request(&self, _ctx: &RequestContext) -> anyhow::Result<Value> {
        Ok(json!({}))
    }

    async fn process_response(&self, ctx: &ResponseContext, response: Value) -> anyhow::Result<()> {
        if response.is_null() || response.as_array().is_some_and(|a| a.is_empty()) {
            return Ok(());
        }
        // Check if response has "range" (DocumentSymbol vs SymbolInformation)
        if let Some(arr) = response.as_array() {
            if !arr.is_empty() && arr[0].get("range").is_none() {
                return Ok(()); // SymbolInformation doesn't have range
            }
        }
        (ctx.eval_in_emacs)("lsp-bridge-breadcrumb--callback", vec![
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
    fn methods_per_lsp_spec() {
        assert_eq!(DocumentSymbol.method(), "textDocument/documentSymbol");
        assert_eq!(IMenu.method(), "textDocument/documentSymbol");
        assert_eq!(Breadcrumb.method(), "textDocument/documentSymbol");
    }
}
