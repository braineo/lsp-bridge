//! Client capabilities construction per LSP 3.17 specification.
//!
//! Reference: https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/
//!
//! We build ClientCapabilities from the spec, then merge with any
//! server-specific overrides from langserver/*.json configs.

use lsp_types::*;
use serde_json::Value;

/// Build default client capabilities per LSP 3.17 spec.
///
/// These are the capabilities that lsp-bridge advertises to LSP servers.
/// Server-specific overrides from config are merged on top.
pub fn default_client_capabilities(enable_diagnostics: bool) -> ClientCapabilities {
    ClientCapabilities {
        workspace: Some(WorkspaceClientCapabilities {
            configuration: Some(true),
            symbol: Some(WorkspaceSymbolClientCapabilities {
                resolve_support: Some(WorkspaceSymbolResolveSupportCapability {
                    properties: vec![],
                }),
                ..Default::default()
            }),
            did_change_watched_files: Some(DidChangeWatchedFilesClientCapabilities {
                dynamic_registration: Some(true),
                relative_pattern_support: Some(true),
            }),
            workspace_folders: Some(true),
            apply_edit: Some(true),
            did_change_configuration: Some(DynamicRegistrationClientCapabilities {
                dynamic_registration: Some(true),
            }),
            ..Default::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            completion: Some(CompletionClientCapabilities {
                completion_item: Some(CompletionItemCapability {
                    snippet_support: Some(true),
                    deprecated_support: Some(true),
                    tag_support: Some(TagSupport {
                        value_set: vec![CompletionItemTag::DEPRECATED],
                    }),
                    resolve_support: Some(CompletionItemCapabilityResolveSupport {
                        // rust-analyzer needs additionalTextEdits for auto-import
                        properties: vec![
                            "documentation".to_string(),
                            "detail".to_string(),
                            "additionalTextEdits".to_string(),
                        ],
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            code_action: Some(CodeActionClientCapabilities {
                dynamic_registration: Some(false),
                code_action_literal_support: Some(CodeActionLiteralSupport {
                    code_action_kind: CodeActionKindLiteralSupport {
                        value_set: vec![
                            "quickfix".to_string(),
                            "refactor".to_string(),
                            "refactor.extract".to_string(),
                            "refactor.inline".to_string(),
                            "refactor.rewrite".to_string(),
                            "source".to_string(),
                            "source.organizeImports".to_string(),
                        ],
                    },
                }),
                is_preferred_support: Some(true),
                ..Default::default()
            }),
            hover: Some(HoverClientCapabilities {
                content_format: Some(vec![
                    MarkupKind::Markdown,
                    MarkupKind::PlainText,
                ]),
                dynamic_registration: Some(true),
            }),
            formatting: Some(DynamicRegistrationClientCapabilities {
                dynamic_registration: Some(true),
            }),
            range_formatting: Some(DynamicRegistrationClientCapabilities {
                dynamic_registration: Some(true),
            }),
            on_type_formatting: Some(DynamicRegistrationClientCapabilities {
                dynamic_registration: Some(true),
            }),
            document_symbol: Some(DocumentSymbolClientCapabilities {
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            inlay_hint: Some(InlayHintClientCapabilities {
                dynamic_registration: Some(false),
                ..Default::default()
            }),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                related_information: Some(enable_diagnostics),
                tag_support: Some(TagSupport {
                    value_set: vec![
                        DiagnosticTag::UNNECESSARY,
                        DiagnosticTag::DEPRECATED,
                    ],
                }),
                version_support: Some(enable_diagnostics),
                code_description_support: Some(enable_diagnostics),
                data_support: Some(enable_diagnostics),
                ..Default::default()
            }),
            diagnostic: Some(DiagnosticClientCapabilities {
                dynamic_registration: Some(false),
                related_document_support: Some(false),
            }),
            signature_help: Some(SignatureHelpClientCapabilities {
                ..Default::default()
            }),
            ..Default::default()
        }),
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Merge server-specific capability overrides from config JSON.
///
/// LSP spec section 3.17: the client capabilities are negotiated,
/// but some servers need custom capability fields (e.g., clangd).
/// Config files can specify a `"capabilities"` object that gets
/// deep-merged into the default capabilities.
pub fn merge_capabilities(
    base: &ClientCapabilities,
    overrides: &Value,
) -> ClientCapabilities {
    if overrides.is_null() || !overrides.is_object() {
        return base.clone();
    }

    // Serialize base to JSON, deep merge overrides, deserialize back
    let mut base_json = serde_json::to_value(base).unwrap_or(Value::Null);
    deep_merge(&mut base_json, overrides);
    serde_json::from_value(base_json).unwrap_or_else(|_| base.clone())
}

/// Deep merge `source` into `target`.
///
/// - Objects: recursively merge keys
/// - Arrays: replace (don't concatenate)
/// - Scalars: source overrides target
fn deep_merge(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, source_val) in source_map {
                let target_val = target_map.entry(key.clone()).or_insert(Value::Null);
                deep_merge(target_val, source_val);
            }
        }
        (target, source) => {
            *target = source.clone();
        }
    }
}

/// Extract server capabilities from an initialize response.
///
/// Per LSP 3.17 spec, the server returns `InitializeResult` which contains
/// `capabilities: ServerCapabilities`.
pub fn extract_server_capabilities(
    init_result: &Value,
) -> Option<ServerCapabilities> {
    init_result
        .get("capabilities")
        .and_then(|caps| serde_json::from_value::<ServerCapabilities>(caps.clone()).ok())
}

/// Parsed server capability flags used for quick checks.
///
/// These are extracted from ServerCapabilities for fast lookup
/// without repeatedly traversing the JSON structure.
#[derive(Debug, Clone)]
pub struct ServerCapabilityFlags {
    pub completion_trigger_characters: Vec<String>,
    pub completion_resolve_provider: bool,
    pub rename_prepare_provider: bool,
    pub code_action_provider: bool,
    pub code_format_provider: bool,
    pub range_format_provider: bool,
    pub document_highlight_provider: bool,
    pub signature_help_provider: bool,
    pub workspace_symbol_provider: bool,
    pub inlay_hint_provider: bool,
    pub semantic_tokens_provider: bool,
    pub diagnostic_provider: bool,
    pub text_document_sync_kind: TextDocumentSyncKind,
    pub save_include_text: bool,
    pub save_provider: bool,
}

impl Default for ServerCapabilityFlags {
    fn default() -> Self {
        Self {
            completion_trigger_characters: Vec::new(),
            completion_resolve_provider: false,
            rename_prepare_provider: false,
            code_action_provider: false,
            code_format_provider: false,
            range_format_provider: false,
            document_highlight_provider: false,
            signature_help_provider: false,
            workspace_symbol_provider: false,
            inlay_hint_provider: false,
            semantic_tokens_provider: false,
            diagnostic_provider: false,
            text_document_sync_kind: TextDocumentSyncKind::INCREMENTAL,
            save_include_text: false,
            save_provider: true,
        }
    }
}

impl ServerCapabilityFlags {
    /// Extract capability flags from LSP ServerCapabilities.
    pub fn from_capabilities(caps: &ServerCapabilities) -> Self {
        let mut flags = Self {
            save_provider: true,
            text_document_sync_kind: TextDocumentSyncKind::INCREMENTAL,
            ..Default::default()
        };

        // Completion
        if let Some(ref provider) = caps.completion_provider {
            flags.completion_trigger_characters = provider
                .trigger_characters
                .clone()
                .unwrap_or_default();
            flags.completion_resolve_provider = provider.resolve_provider.unwrap_or(false);
        }

        // Rename
        if let Some(ref provider) = caps.rename_provider {
            match provider {
                OneOf::Left(b) => flags.rename_prepare_provider = *b,
                OneOf::Right(opts) => {
                    flags.rename_prepare_provider = opts.prepare_provider.unwrap_or(false);
                }
            }
        }

        // Code action
        if let Some(ref provider) = caps.code_action_provider {
            match provider {
                CodeActionProviderCapability::Simple(b) => flags.code_action_provider = *b,
                CodeActionProviderCapability::Options(_) => flags.code_action_provider = true,
            }
        }

        // Formatting
        if let Some(ref provider) = caps.document_formatting_provider {
            match provider {
                OneOf::Left(b) => flags.code_format_provider = *b,
                OneOf::Right(_) => flags.code_format_provider = true,
            }
        }

        // Range formatting
        if let Some(ref provider) = caps.document_range_formatting_provider {
            match provider {
                OneOf::Left(b) => flags.range_format_provider = *b,
                OneOf::Right(_) => flags.range_format_provider = true,
            }
        }

        // Document highlight
        if let Some(ref provider) = caps.document_highlight_provider {
            match provider {
                OneOf::Left(b) => flags.document_highlight_provider = *b,
                OneOf::Right(_) => flags.document_highlight_provider = true,
            }
        }

        // Signature help
        if caps.signature_help_provider.is_some() {
            flags.signature_help_provider = true;
        }

        // Workspace symbol
        if let Some(ref provider) = caps.workspace_symbol_provider {
            match provider {
                OneOf::Left(b) => flags.workspace_symbol_provider = *b,
                OneOf::Right(_) => flags.workspace_symbol_provider = true,
            }
        }

        // Inlay hints (OneOf<bool, InlayHintServerCapabilities>)
        if let Some(ref provider) = caps.inlay_hint_provider {
            match provider {
                OneOf::Left(b) => flags.inlay_hint_provider = *b,
                OneOf::Right(_) => flags.inlay_hint_provider = true,
            }
        }

        // Semantic tokens
        if caps.semantic_tokens_provider.is_some() {
            flags.semantic_tokens_provider = true;
        }

        // Diagnostics (pull-based, LSP 3.17)
        if caps.diagnostic_provider.is_some() {
            flags.diagnostic_provider = true;
        }

        // Text document sync (LSP 3.17 §3.4.1)
        if let Some(ref sync) = caps.text_document_sync {
            match sync {
                TextDocumentSyncCapability::Kind(kind) => {
                    flags.text_document_sync_kind = *kind;
                }
                TextDocumentSyncCapability::Options(opts) => {
                    if let Some(change) = opts.change {
                        flags.text_document_sync_kind = change;
                    }
                    // Save capability (LSP 3.17 §3.4.5)
                    if let Some(ref save) = opts.save {
                        match save {
                            TextDocumentSyncSaveOptions::Supported(b) => {
                                flags.save_provider = *b;
                            }
                            TextDocumentSyncSaveOptions::SaveOptions(opts) => {
                                flags.save_provider = true;
                                flags.save_include_text = opts.include_text.unwrap_or(false);
                            }
                        }
                    }
                }
            }
        }

        flags
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_capabilities_has_completion() {
        let caps = default_client_capabilities(true);
        let td = caps.text_document.unwrap();
        assert!(td.completion.is_some());
        let comp = td.completion.unwrap();
        let item = comp.completion_item.unwrap();
        assert!(item.snippet_support.unwrap());
    }

    #[test]
    fn default_capabilities_has_hover_markdown() {
        let caps = default_client_capabilities(true);
        let td = caps.text_document.unwrap();
        let hover = td.hover.unwrap();
        let formats = hover.content_format.unwrap();
        assert!(formats.contains(&MarkupKind::Markdown));
    }

    #[test]
    fn default_capabilities_has_code_action() {
        let caps = default_client_capabilities(true);
        let td = caps.text_document.unwrap();
        let ca = td.code_action.unwrap();
        assert!(!ca.dynamic_registration.unwrap());
        let literal = ca.code_action_literal_support.unwrap();
        assert!(literal.code_action_kind.value_set.contains(&"quickfix".to_string()));
    }

    #[test]
    fn default_capabilities_diagnostics_enabled() {
        let caps = default_client_capabilities(true);
        let td = caps.text_document.unwrap();
        let pd = td.publish_diagnostics.unwrap();
        assert!(pd.related_information.unwrap());
        assert!(pd.version_support.unwrap());
    }

    #[test]
    fn default_capabilities_diagnostics_disabled() {
        let caps = default_client_capabilities(false);
        let td = caps.text_document.unwrap();
        let pd = td.publish_diagnostics.unwrap();
        assert!(!pd.related_information.unwrap());
        assert!(!pd.version_support.unwrap());
    }

    #[test]
    fn default_capabilities_workspace_folders() {
        let caps = default_client_capabilities(true);
        let ws = caps.workspace.unwrap();
        assert!(ws.workspace_folders.unwrap());
        assert!(ws.configuration.unwrap());
    }

    #[test]
    fn default_capabilities_work_done_progress() {
        let caps = default_client_capabilities(true);
        let win = caps.window.unwrap();
        assert!(win.work_done_progress.unwrap());
    }

    #[test]
    fn default_capabilities_serializes_to_valid_json() {
        let caps = default_client_capabilities(true);
        let json = serde_json::to_value(&caps).unwrap();
        assert!(json.is_object());
        assert!(json["textDocument"]["completion"]["completionItem"]["snippetSupport"].as_bool().unwrap());
        assert!(json["workspace"]["configuration"].as_bool().unwrap());
    }

    #[test]
    fn merge_capabilities_with_empty_overrides() {
        let caps = default_client_capabilities(true);
        let merged = merge_capabilities(&caps, &Value::Null);
        // Should be identical
        let orig_json = serde_json::to_value(&caps).unwrap();
        let merged_json = serde_json::to_value(&merged).unwrap();
        assert_eq!(orig_json, merged_json);
    }

    #[test]
    fn merge_capabilities_with_overrides() {
        let caps = default_client_capabilities(true);
        let overrides = serde_json::json!({
            "textDocument": {
                "hover": {
                    "contentFormat": ["markdown"]
                }
            }
        });
        let merged = merge_capabilities(&caps, &overrides);
        let json = serde_json::to_value(&merged).unwrap();
        // Override should have taken effect
        let formats = json["textDocument"]["hover"]["contentFormat"].as_array().unwrap();
        assert_eq!(formats.len(), 1);
        assert_eq!(formats[0], "markdown");
    }

    #[test]
    fn deep_merge_objects() {
        let mut target = serde_json::json!({"a": 1, "b": {"c": 2}});
        let source = serde_json::json!({"b": {"d": 3}, "e": 4});
        deep_merge(&mut target, &source);
        assert_eq!(target["a"], 1);
        assert_eq!(target["b"]["c"], 2);
        assert_eq!(target["b"]["d"], 3);
        assert_eq!(target["e"], 4);
    }

    #[test]
    fn deep_merge_replaces_scalars() {
        let mut target = serde_json::json!({"a": 1});
        let source = serde_json::json!({"a": 2});
        deep_merge(&mut target, &source);
        assert_eq!(target["a"], 2);
    }

    #[test]
    fn extract_capabilities_from_init_result() {
        // Simulated pyright initialize response
        let init_result = serde_json::json!({
            "capabilities": {
                "completionProvider": {
                    "triggerCharacters": [".", "[", "\"", "'"],
                    "resolveProvider": true
                },
                "textDocumentSync": {
                    "openClose": true,
                    "change": 2,
                    "save": {"includeText": false}
                },
                "hoverProvider": true,
                "definitionProvider": true,
                "codeActionProvider": {"codeActionKinds": ["quickfix", "source.organizeImports"]},
                "documentFormattingProvider": true
            }
        });

        let caps = extract_server_capabilities(&init_result).unwrap();
        let flags = ServerCapabilityFlags::from_capabilities(&caps);

        assert_eq!(flags.completion_trigger_characters, vec![".", "[", "\"", "'"]);
        assert!(flags.completion_resolve_provider);
        assert_eq!(flags.text_document_sync_kind, TextDocumentSyncKind::INCREMENTAL);
        assert!(!flags.save_include_text);
        assert!(flags.code_action_provider);
        assert!(flags.code_format_provider);
    }

    #[test]
    fn extract_capabilities_rust_analyzer() {
        // Simulated rust-analyzer initialize response subset
        let init_result = serde_json::json!({
            "capabilities": {
                "completionProvider": {
                    "triggerCharacters": [".", ":", "'", "("],
                    "resolveProvider": true
                },
                "textDocumentSync": 2,
                "inlayHintProvider": {"resolveProvider": true},
                "semanticTokensProvider": {"full": {"delta": true}, "legend": {"tokenTypes": [], "tokenModifiers": []}},
                "signatureHelpProvider": {"triggerCharacters": ["(", ","]},
                "renameProvider": {"prepareProvider": true},
                "documentHighlightProvider": true,
                "codeActionProvider": true
            }
        });

        let caps = extract_server_capabilities(&init_result).unwrap();
        let flags = ServerCapabilityFlags::from_capabilities(&caps);

        assert!(flags.completion_resolve_provider);
        assert_eq!(flags.text_document_sync_kind, TextDocumentSyncKind::INCREMENTAL);
        assert!(flags.inlay_hint_provider);
        assert!(flags.semantic_tokens_provider);
        assert!(flags.signature_help_provider);
        assert!(flags.rename_prepare_provider);
        assert!(flags.document_highlight_provider);
        assert!(flags.code_action_provider);
    }

    #[test]
    fn extract_capabilities_no_sync() {
        let init_result = serde_json::json!({
            "capabilities": {
                "textDocumentSync": 0
            }
        });

        let caps = extract_server_capabilities(&init_result).unwrap();
        let flags = ServerCapabilityFlags::from_capabilities(&caps);
        assert_eq!(flags.text_document_sync_kind, TextDocumentSyncKind::NONE);
    }

    #[test]
    fn extract_capabilities_full_sync() {
        let init_result = serde_json::json!({
            "capabilities": {
                "textDocumentSync": 1
            }
        });

        let caps = extract_server_capabilities(&init_result).unwrap();
        let flags = ServerCapabilityFlags::from_capabilities(&caps);
        assert_eq!(flags.text_document_sync_kind, TextDocumentSyncKind::FULL);
    }

    #[test]
    fn capability_flags_default() {
        let flags = ServerCapabilityFlags::default();
        assert!(!flags.completion_resolve_provider);
        assert!(!flags.code_action_provider);
        assert!(flags.completion_trigger_characters.is_empty());
    }
}
