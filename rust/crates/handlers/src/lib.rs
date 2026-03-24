//! LSP request handler trait and implementations.
//!
//! Each handler maps to a specific LSP method (e.g., textDocument/completion).
//! The trait mirrors Python's Handler ABC from core/handler/__init__.py:
//!
//! - `name`: EPC method name called from Emacs
//! - `method`: LSP protocol method name
//! - `cancel_on_change`: discard response if file/cursor changed
//! - `send_document_uri`: auto-add textDocument.uri to params
//! - `process_request`: build LSP params from Emacs args
//! - `process_response`: process LSP response, call back to Emacs

pub mod completion;
pub mod completion_item;
pub mod hover;
pub mod find_define;
pub mod code_action;
pub mod formatting;
pub mod rename;
pub mod signature_help;
pub mod inlay_hint;
pub mod semantic_tokens;
pub mod diagnostic;
pub mod document_symbol;
pub mod document_highlight;
pub mod execute_command;
pub mod workspace_symbol;
pub mod call_hierarchy;

use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use serde_json::Value;

/// LSP CompletionItemKind → display string mapping.
/// Per LSP 3.17 §3.18.3, index matches CompletionItemKind enum values.
///
/// Ground truth from Python: core/utils.py KIND_MAP
pub const KIND_MAP: &[&str] = &[
    "",             // 0: unknown
    "Text",         // 1
    "Method",       // 2
    "Function",     // 3
    "Constructor",  // 4
    "Field",        // 5
    "Variable",     // 6
    "Class",        // 7
    "Interface",    // 8
    "Module",       // 9
    "Property",     // 10
    "Unit",         // 11
    "Value",        // 12
    "Enum",         // 13
    "Keyword",      // 14
    "Snippet",      // 15
    "Color",        // 16
    "File",         // 17
    "Reference",    // 18
    "Folder",       // 19
    "EnumMember",   // 20
    "Constant",     // 21
    "Struct",       // 22
    "Event",        // 23
    "Operator",     // 24
    "TypeParameter", // 25
];

/// LSP SymbolKind → display string mapping.
/// Per LSP 3.17 §3.18.2.
pub const SYMBOL_MAP: &[&str] = &[
    "",             // 0: unknown
    "File",         // 1
    "Module",       // 2
    "Namespace",    // 3
    "Package",      // 4
    "Class",        // 5
    "Method",       // 6
    "Property",     // 7
    "Field",        // 8
    "Constructor",  // 9
    "Enum",         // 10
    "Interface",    // 11
    "Function",     // 12
    "Variable",     // 13
    "Constant",     // 14
    "String",       // 15
    "Number",       // 16
    "Boolean",      // 17
    "Array",        // 18
    "Object",       // 19
    "Key",          // 20
    "Null",         // 21
    "EnumMember",   // 22
    "Struct",       // 23
    "Event",        // 24
    "Operator",     // 25
    "TypeParameter", // 26
];

/// Get the kind string for a CompletionItemKind value.
pub fn kind_name(kind: u64) -> &'static str {
    KIND_MAP.get(kind as usize).copied().unwrap_or("")
}

/// The Handler trait — direct mapping from Python's Handler ABC.
///
/// Each handler is stateless; per-request state is passed via HandlerContext.
/// This differs from Python where handler instances hold mutable state.
#[async_trait]
pub trait Handler: Send + Sync {
    /// Name used by Emacs to call this handler (e.g., "completion").
    fn name(&self) -> &'static str;

    /// LSP method name (e.g., "textDocument/completion").
    fn method(&self) -> &'static str;

    /// Whether to discard the response if the file or cursor changed.
    fn cancel_on_change(&self) -> bool {
        false
    }

    /// Whether to auto-add textDocument.uri to request params.
    fn send_document_uri(&self) -> bool {
        true
    }

    /// Build LSP request params from the arguments received from Emacs.
    ///
    /// Returns the params object to send in the JSON-RPC request.
    fn process_request(
        &self,
        ctx: &RequestContext,
    ) -> anyhow::Result<Value>;

    /// Process the LSP server response.
    ///
    /// Called when the server returns a result. Should call back to Emacs
    /// via the response context (e.g., eval_in_emacs).
    async fn process_response(
        &self,
        ctx: &ResponseContext,
        response: Value,
    ) -> anyhow::Result<()>;
}

/// Context passed to process_request.
pub struct RequestContext {
    /// Arguments from Emacs (varies per handler).
    pub args: Vec<Value>,
    /// Server name for this request.
    pub server_name: String,
    /// Completion trigger characters from server capabilities.
    pub trigger_characters: Vec<String>,
    /// Server info from config.
    pub server_info: Value,
}

/// Context passed to process_response.
pub struct ResponseContext {
    /// File path being operated on.
    pub filepath: String,
    /// LSP file host (for remote connections).
    pub host: String,
    /// Server name.
    pub server_name: String,
    /// Trigger characters.
    pub trigger_characters: Vec<String>,
    /// All server names for this file.
    pub server_names: Vec<String>,
    /// Callback to send results to Emacs.
    /// Signature: (method_name, args_json)
    pub eval_in_emacs: Box<dyn Fn(&str, Vec<Value>) + Send + Sync>,
    /// Callback to send message to Emacs.
    pub message_emacs: Box<dyn Fn(&str) + Send + Sync>,
}

/// Per-request handler state for staleness checking.
///
/// Mirrors Python's Handler instance state (latest_request_id, last_change).
pub struct HandlerState {
    pub latest_request_id: AtomicI64,
    pub last_change_file_time: std::sync::atomic::AtomicU64,
    pub last_change_cursor_time: std::sync::atomic::AtomicU64,
}

impl HandlerState {
    pub fn new() -> Self {
        Self {
            latest_request_id: AtomicI64::new(-1),
            last_change_file_time: std::sync::atomic::AtomicU64::new(0),
            last_change_cursor_time: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Record the current request ID and change times.
    pub fn record(&self, request_id: i64, file_time: u64, cursor_time: u64) {
        self.latest_request_id.store(request_id, Ordering::SeqCst);
        self.last_change_file_time.store(file_time, Ordering::SeqCst);
        self.last_change_cursor_time.store(cursor_time, Ordering::SeqCst);
    }

    /// Check if a response is still fresh (not superseded by a newer request).
    ///
    /// Mirrors Python's handle_response() staleness check.
    pub fn is_fresh(
        &self,
        request_id: i64,
        cancel_on_change: bool,
        current_file_time: u64,
        current_cursor_time: u64,
    ) -> bool {
        // Check if request ID matches latest
        if request_id != self.latest_request_id.load(Ordering::SeqCst) {
            return false;
        }

        // Check if file/cursor changed (if handler cares)
        if cancel_on_change {
            let stored_file = self.last_change_file_time.load(Ordering::SeqCst);
            let stored_cursor = self.last_change_cursor_time.load(Ordering::SeqCst);
            if stored_file != current_file_time || stored_cursor != current_cursor_time {
                return false;
            }
        }

        true
    }
}

impl Default for HandlerState {
    fn default() -> Self {
        Self::new()
    }
}

/// Create all built-in handlers.
pub fn create_all_handlers() -> Vec<Box<dyn Handler>> {
    vec![
        Box::new(completion::Completion),
        Box::new(completion_item::CompletionItem),
        Box::new(hover::Hover),
        Box::new(find_define::FindDefine),
        Box::new(find_define::FindTypeDefine),
        Box::new(find_define::FindImplementation),
        Box::new(find_define::FindReferences),
        Box::new(code_action::CodeAction),
        Box::new(formatting::Formatting),
        Box::new(formatting::RangeFormatting),
        Box::new(rename::PrepareRename),
        Box::new(rename::Rename),
        Box::new(signature_help::SignatureHelp),
        Box::new(inlay_hint::InlayHint),
        Box::new(semantic_tokens::SemanticTokens),
        Box::new(diagnostic::Diagnostic),
        Box::new(document_symbol::DocumentSymbol),
        Box::new(document_symbol::IMenu),
        Box::new(document_symbol::Breadcrumb),
        Box::new(document_highlight::DocumentHighlight),
        Box::new(execute_command::ExecuteCommand),
        Box::new(workspace_symbol::WorkspaceSymbol),
        Box::new(call_hierarchy::PrepareCallHierarchyIncoming),
        Box::new(call_hierarchy::PrepareCallHierarchyOutgoing),
        Box::new(call_hierarchy::CallHierarchyIncoming),
        Box::new(call_hierarchy::CallHierarchyOutgoing),
    ]
}

/// Build a handler registry mapping name → handler.
pub fn build_registry() -> std::collections::HashMap<&'static str, Box<dyn Handler>> {
    let mut map = std::collections::HashMap::new();
    for handler in create_all_handlers() {
        map.insert(handler.name(), handler);
    }
    map
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Python ground truth: KIND_MAP from core/utils.py
    #[test]
    fn kind_map_matches_python() {
        let python_kind_map = vec![
            "", "Text", "Method", "Function", "Constructor", "Field",
            "Variable", "Class", "Interface", "Module", "Property",
            "Unit", "Value", "Enum", "Keyword", "Snippet", "Color",
            "File", "Reference", "Folder", "EnumMember", "Constant",
            "Struct", "Event", "Operator", "TypeParameter",
        ];
        assert_eq!(KIND_MAP.len(), python_kind_map.len());
        for (i, expected) in python_kind_map.iter().enumerate() {
            assert_eq!(KIND_MAP[i], *expected, "mismatch at index {}", i);
        }
    }

    #[test]
    fn symbol_map_matches_python() {
        let python_symbol_map = vec![
            "", "File", "Module", "Namespace", "Package",
            "Class", "Method", "Property", "Field", "Constructor",
            "Enum", "Interface", "Function", "Variable", "Constant",
            "String", "Number", "Boolean", "Array", "Object",
            "Key", "Null", "EnumMember", "Struct", "Event",
            "Operator", "TypeParameter",
        ];
        assert_eq!(SYMBOL_MAP.len(), python_symbol_map.len());
        for (i, expected) in python_symbol_map.iter().enumerate() {
            assert_eq!(SYMBOL_MAP[i], *expected, "mismatch at index {}", i);
        }
    }

    #[test]
    fn kind_name_valid() {
        assert_eq!(kind_name(3), "Function");
        assert_eq!(kind_name(15), "Snippet");
        assert_eq!(kind_name(0), "");
    }

    #[test]
    fn kind_name_out_of_range() {
        assert_eq!(kind_name(999), "");
    }

    #[test]
    fn handler_state_freshness() {
        let state = HandlerState::new();
        state.record(42, 100, 200);

        // Same request_id, same times → fresh
        assert!(state.is_fresh(42, false, 100, 200));
        assert!(state.is_fresh(42, true, 100, 200));

        // Different request_id → stale
        assert!(!state.is_fresh(41, false, 100, 200));

        // Same request_id but file changed, cancel_on_change=true → stale
        assert!(!state.is_fresh(42, true, 101, 200));
        assert!(!state.is_fresh(42, true, 100, 201));

        // Same request_id but file changed, cancel_on_change=false → still fresh
        assert!(state.is_fresh(42, false, 101, 200));
    }

    #[test]
    fn registry_has_all_handlers() {
        let registry = build_registry();
        let expected = [
            "completion", "completion_item_resolve", "hover",
            "find_define", "find_type_define", "find_implementation", "find_references",
            "code_action", "formatting", "rangeFormatting",
            "prepare_rename", "rename", "signature_help",
            "inlay_hint", "semantic_tokens", "diagnostic",
            "document_symbol", "imenu", "breadcrumb",
            "document_highlight", "execute_command", "workspace_symbol",
            "prepare_call_hierarchy_incoming", "prepare_call_hierarchy_outgoing",
            "call_hierarchy_incoming", "call_hierarchy_outgoing",
        ];
        for name in &expected {
            assert!(registry.contains_key(name), "missing handler: {}", name);
        }
        assert_eq!(registry.len(), expected.len());
    }

    #[test]
    fn handler_methods_match_lsp_spec() {
        let registry = build_registry();
        // Per LSP 3.17 spec method names
        let expected_methods = [
            ("completion", "textDocument/completion"),
            ("completion_item_resolve", "completionItem/resolve"),
            ("hover", "textDocument/hover"),
            ("find_define", "textDocument/definition"),
            ("find_type_define", "textDocument/typeDefinition"),
            ("find_implementation", "textDocument/implementation"),
            ("find_references", "textDocument/references"),
            ("code_action", "textDocument/codeAction"),
            ("formatting", "textDocument/formatting"),
            ("rangeFormatting", "textDocument/rangeFormatting"),
            ("prepare_rename", "textDocument/prepareRename"),
            ("rename", "textDocument/rename"),
            ("signature_help", "textDocument/signatureHelp"),
            ("inlay_hint", "textDocument/inlayHint"),
            ("semantic_tokens", "textDocument/semanticTokens/full"),
            ("diagnostic", "textDocument/diagnostic"),
            ("document_symbol", "textDocument/documentSymbol"),
            ("document_highlight", "textDocument/documentHighlight"),
            ("execute_command", "workspace/executeCommand"),
            ("workspace_symbol", "workspace/symbol"),
            ("prepare_call_hierarchy_incoming", "textDocument/prepareCallHierarchy"),
            ("call_hierarchy_incoming", "callHierarchy/incomingCalls"),
            ("call_hierarchy_outgoing", "callHierarchy/outgoingCalls"),
        ];
        for (name, method) in &expected_methods {
            assert_eq!(
                registry[name].method(), *method,
                "method mismatch for handler '{}'", name
            );
        }
    }
}
