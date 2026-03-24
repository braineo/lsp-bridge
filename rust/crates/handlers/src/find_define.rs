//! Navigation handlers — textDocument/definition, typeDefinition, implementation, references.
//!
//! LSP 3.17 §3.18.5-3.18.8.
//! Mirrors Python's core/handler/find_define.py and related handlers.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::{Handler, RequestContext, ResponseContext};
use lsp_server::server::{path_to_uri, uri_to_path};

// ---------------------------------------------------------------------------
// textDocument/definition (§3.18.5)
// ---------------------------------------------------------------------------

pub struct FindDefine;

#[async_trait]
impl Handler for FindDefine {
    fn name(&self) -> &'static str {
        "find_define"
    }
    fn method(&self) -> &'static str {
        "textDocument/definition"
    }
    fn cancel_on_change(&self) -> bool {
        true
    }

    /// Args: [position]
    /// Python ground truth: dict(position=position)
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let position = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        Ok(json!({"position": position}))
    }

    /// LSP 3.17: response is Location | Location[] | LocationLink[] | null
    async fn process_response(
        &self,
        ctx: &ResponseContext,
        response: Value,
    ) -> anyhow::Result<()> {
        handle_location_response(ctx, response, "lsp-bridge-define--jump")
    }
}

// ---------------------------------------------------------------------------
// textDocument/typeDefinition (§3.18.6)
// ---------------------------------------------------------------------------

pub struct FindTypeDefine;

#[async_trait]
impl Handler for FindTypeDefine {
    fn name(&self) -> &'static str {
        "find_type_define"
    }
    fn method(&self) -> &'static str {
        "textDocument/typeDefinition"
    }
    fn cancel_on_change(&self) -> bool {
        true
    }
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let position = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        Ok(json!({"position": position}))
    }
    async fn process_response(
        &self,
        ctx: &ResponseContext,
        response: Value,
    ) -> anyhow::Result<()> {
        handle_location_response(ctx, response, "lsp-bridge-define--jump")
    }
}

// ---------------------------------------------------------------------------
// textDocument/implementation (§3.18.7)
// ---------------------------------------------------------------------------

pub struct FindImplementation;

#[async_trait]
impl Handler for FindImplementation {
    fn name(&self) -> &'static str {
        "find_implementation"
    }
    fn method(&self) -> &'static str {
        "textDocument/implementation"
    }
    fn cancel_on_change(&self) -> bool {
        true
    }
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let position = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        Ok(json!({"position": position}))
    }
    async fn process_response(
        &self,
        ctx: &ResponseContext,
        response: Value,
    ) -> anyhow::Result<()> {
        handle_location_response(ctx, response, "lsp-bridge-define--jump")
    }
}

// ---------------------------------------------------------------------------
// textDocument/references (§3.18.8)
// ---------------------------------------------------------------------------

pub struct FindReferences;

#[async_trait]
impl Handler for FindReferences {
    fn name(&self) -> &'static str {
        "find_references"
    }
    fn method(&self) -> &'static str {
        "textDocument/references"
    }
    fn cancel_on_change(&self) -> bool {
        true
    }

    /// Args: [position]
    /// Per LSP 3.17: ReferenceParams includes context.includeDeclaration
    fn process_request(&self, ctx: &RequestContext) -> anyhow::Result<Value> {
        let position = ctx.args.get(0).cloned().unwrap_or(Value::Null);
        Ok(json!({
            "position": position,
            "context": {"includeDeclaration": true}
        }))
    }

    async fn process_response(
        &self,
        ctx: &ResponseContext,
        response: Value,
    ) -> anyhow::Result<()> {
        // References returns Location[] | null
        if response.is_null() {
            (ctx.message_emacs)("No references found.");
            return Ok(());
        }

        let locations = response.as_array().cloned().unwrap_or_default();

        let file_infos: Vec<Value> = locations
            .iter()
            .filter_map(|loc| extract_location(loc))
            .map(|(path, line, col)| {
                json!({"filepath": path, "line": line, "character": col})
            })
            .collect();

        if file_infos.is_empty() {
            (ctx.message_emacs)("No references found.");
        } else {
            (ctx.eval_in_emacs)(
                "lsp-bridge-references--popup",
                vec![Value::Array(file_infos)],
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Shared location response handling
// ---------------------------------------------------------------------------

/// Handle a definition/typeDefinition/implementation response.
///
/// Per LSP 3.17, response is: Location | Location[] | LocationLink[] | null
fn handle_location_response(
    ctx: &ResponseContext,
    response: Value,
    emacs_callback: &str,
) -> anyhow::Result<()> {
    if response.is_null() {
        (ctx.message_emacs)("No definition found.");
        return Ok(());
    }

    // Single location
    if response.is_object() {
        if let Some((path, line, col)) = extract_location(&response) {
            (ctx.eval_in_emacs)(
                emacs_callback,
                vec![
                    Value::String(path),
                    Value::String(ctx.host.clone()),
                    json!({"line": line, "character": col}),
                ],
            );
            return Ok(());
        }
    }

    // Array of locations or location links
    if let Some(arr) = response.as_array() {
        if arr.is_empty() {
            (ctx.message_emacs)("No definition found.");
            return Ok(());
        }

        // Use first location
        let first = &arr[0];
        if let Some((path, line, col)) = extract_location(first) {
            (ctx.eval_in_emacs)(
                emacs_callback,
                vec![
                    Value::String(path),
                    Value::String(ctx.host.clone()),
                    json!({"line": line, "character": col}),
                ],
            );
            return Ok(());
        }

        // Try LocationLink format (§3.18.5)
        if let Some((path, line, col)) = extract_location_link(first) {
            (ctx.eval_in_emacs)(
                emacs_callback,
                vec![
                    Value::String(path),
                    Value::String(ctx.host.clone()),
                    json!({"line": line, "character": col}),
                ],
            );
            return Ok(());
        }
    }

    (ctx.message_emacs)("No definition found.");
    Ok(())
}

/// Extract filepath, line, column from a Location object.
///
/// LSP 3.17 Location = { uri: DocumentUri, range: Range }
fn extract_location(loc: &Value) -> Option<(String, u64, u64)> {
    let uri = loc.get("uri")?.as_str()?;
    let range = loc.get("range")?;
    let start = range.get("start")?;
    let line = start.get("line")?.as_u64()?;
    let col = start.get("character")?.as_u64()?;
    Some((uri_to_path(uri), line, col))
}

/// Extract filepath, line, column from a LocationLink object.
///
/// LSP 3.17 LocationLink = { targetUri, targetRange, targetSelectionRange, originSelectionRange? }
fn extract_location_link(link: &Value) -> Option<(String, u64, u64)> {
    let uri = link.get("targetUri")?.as_str()?;
    let range = link.get("targetSelectionRange")
        .or_else(|| link.get("targetRange"))?;
    let start = range.get("start")?;
    let line = start.get("line")?.as_u64()?;
    let col = start.get("character")?.as_u64()?;
    Some((uri_to_path(uri), line, col))
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

    // Python ground truth: dict(position=position)
    #[test]
    fn find_define_request() {
        let handler = FindDefine;
        let ctx = make_ctx(vec![json!({"line": 5, "character": 10})]);
        let params = handler.process_request(&ctx).unwrap();
        assert_eq!(
            params,
            json!({"position": {"line": 5, "character": 10}})
        );
    }

    #[test]
    fn find_references_request() {
        let handler = FindReferences;
        let ctx = make_ctx(vec![json!({"line": 5, "character": 10})]);
        let params = handler.process_request(&ctx).unwrap();
        assert_eq!(params["position"]["line"], 5);
        assert_eq!(params["context"]["includeDeclaration"], true);
    }

    // LSP 3.17: Location
    #[test]
    fn extract_location_basic() {
        let loc = json!({
            "uri": "file:///tmp/test.py",
            "range": {
                "start": {"line": 10, "character": 5},
                "end": {"line": 10, "character": 15}
            }
        });
        let (path, line, col) = extract_location(&loc).unwrap();
        assert_eq!(path, "/tmp/test.py");
        assert_eq!(line, 10);
        assert_eq!(col, 5);
    }

    // LSP 3.17: LocationLink (used by volar, etc.)
    #[test]
    fn extract_location_link_basic() {
        let link = json!({
            "targetUri": "file:///tmp/other.py",
            "targetRange": {
                "start": {"line": 20, "character": 0},
                "end": {"line": 25, "character": 0}
            },
            "targetSelectionRange": {
                "start": {"line": 20, "character": 4},
                "end": {"line": 20, "character": 10}
            }
        });
        let (path, line, col) = extract_location_link(&link).unwrap();
        assert_eq!(path, "/tmp/other.py");
        assert_eq!(line, 20);
        assert_eq!(col, 4); // Uses targetSelectionRange
    }

    #[test]
    fn extract_location_link_no_selection_range() {
        let link = json!({
            "targetUri": "file:///tmp/other.py",
            "targetRange": {
                "start": {"line": 20, "character": 0},
                "end": {"line": 25, "character": 0}
            }
        });
        let (path, line, col) = extract_location_link(&link).unwrap();
        assert_eq!(path, "/tmp/other.py");
        assert_eq!(line, 20);
        assert_eq!(col, 0); // Falls back to targetRange
    }

    #[test]
    fn handler_methods_match_lsp_spec() {
        assert_eq!(FindDefine.method(), "textDocument/definition");
        assert_eq!(FindTypeDefine.method(), "textDocument/typeDefinition");
        assert_eq!(FindImplementation.method(), "textDocument/implementation");
        assert_eq!(FindReferences.method(), "textDocument/references");
    }

    #[test]
    fn all_cancel_on_change() {
        assert!(FindDefine.cancel_on_change());
        assert!(FindTypeDefine.cancel_on_change());
        assert!(FindImplementation.cancel_on_change());
        assert!(FindReferences.cancel_on_change());
    }
}
