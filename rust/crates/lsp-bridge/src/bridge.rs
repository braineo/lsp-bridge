//! LspBridge orchestrator — replaces lsp_bridge.py.
//!
//! Manages the lifecycle:
//! 1. Start EPC server, connect to Emacs
//! 2. Register EPC methods (open_file, close_file, change_file, etc.)
//! 3. Open files → create FileAction + spawn LSP servers
//! 4. Route EPC calls → FileAction → Handler → LspServer → Emacs callback

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing;

use epc::client::EpcClient;
use epc::server::EpcServer;
use epc::sexp::SexpValue;
use epc::types::{EvalArg, EpcMessage};

use lsp_server::server::LspServer;
use lsp_server::capabilities::ServerCapabilityFlags;

use handlers::{self, Handler, RequestContext, ResponseContext, HandlerState};

use crate::config::{self, ServerConfig, MultiServerConfig};

/// File action: manages a single open file's LSP server(s) and handlers.
pub struct FileAction {
    pub filepath: String,
    pub version: std::sync::atomic::AtomicU64,
    pub last_change_file_time: std::sync::atomic::AtomicU64,
    pub last_change_cursor_time: std::sync::atomic::AtomicU64,

    /// Single-server mode
    pub single_server: Option<Arc<LspServer>>,
    pub single_server_info: Option<ServerConfig>,

    /// Multi-server mode
    pub multi_servers: Option<HashMap<String, Arc<LspServer>>>,
    pub multi_servers_info: Option<MultiServerConfig>,

    /// Per-(server, handler) state for staleness tracking
    pub handler_states: DashMap<(String, String), HandlerState>,
}

impl FileAction {
    pub fn new(filepath: String) -> Self {
        Self {
            filepath,
            version: std::sync::atomic::AtomicU64::new(0),
            last_change_file_time: std::sync::atomic::AtomicU64::new(0),
            last_change_cursor_time: std::sync::atomic::AtomicU64::new(0),
            single_server: None,
            single_server_info: None,
            multi_servers: None,
            multi_servers_info: None,
            handler_states: DashMap::new(),
        }
    }

    /// Get the LSP servers for this file.
    pub fn get_lsp_servers(&self) -> Vec<Arc<LspServer>> {
        if let Some(ref server) = self.single_server {
            vec![server.clone()]
        } else if let Some(ref servers) = self.multi_servers {
            servers.values().cloned().collect()
        } else {
            vec![]
        }
    }

    /// Get server names.
    pub fn get_server_names(&self) -> Vec<String> {
        self.get_lsp_servers()
            .iter()
            .map(|s| s.server_name.clone())
            .collect()
    }
}

/// The main LspBridge orchestrator.
pub struct LspBridge {
    /// EPC client for calling back to Emacs.
    pub emacs: Option<Arc<EpcClient>>,

    /// Open file actions, keyed by filepath.
    pub file_actions: DashMap<String, Arc<FileAction>>,

    /// LSP server pool, keyed by "server_name#project_path".
    pub lsp_servers: DashMap<String, Arc<LspServer>>,

    /// Handler registry.
    pub handlers: HashMap<&'static str, Box<dyn Handler>>,

    /// Server config directory paths.
    pub langserver_dir: PathBuf,
    pub multiserver_dir: PathBuf,
}

impl LspBridge {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            emacs: None,
            file_actions: DashMap::new(),
            lsp_servers: DashMap::new(),
            handlers: handlers::build_registry(),
            langserver_dir: base_dir.join("langserver"),
            multiserver_dir: base_dir.join("multiserver"),
        }
    }

    /// Set the Emacs EPC client after connection is established.
    pub fn set_emacs(&mut self, client: EpcClient) {
        self.emacs = Some(Arc::new(client));
    }

    /// Get a reference to the Emacs client.
    pub fn emacs(&self) -> &Arc<EpcClient> {
        self.emacs.as_ref().expect("Emacs client not initialized")
    }

    /// Open a file — find/create LSP server, create FileAction.
    ///
    /// Mirrors Python's open_file() in lsp_bridge.py.
    pub async fn open_file(&self, filepath: &str) -> Result<bool> {
        if self.file_actions.contains_key(filepath) {
            return Ok(true); // already open
        }

        let filepath_path = Path::new(filepath);
        let project_path = find_project_path(filepath_path);

        // TODO: query Emacs for multi/single server config via EPC
        // For now, use a simple heuristic based on file extension
        let extension = filepath_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        let server_config = self.find_server_config(extension)?;

        match server_config {
            Some(config) => {
                let server_key = format!("{}#{}", config.name, project_path.display());

                // Reuse existing server or create new one
                let lsp_server = if let Some(server) = self.lsp_servers.get(&server_key) {
                    server.clone()
                } else {
                    let server = self.create_lsp_server(
                        &config,
                        &project_path,
                        true, // enable_diagnostics
                    ).await?;
                    self.lsp_servers.insert(server_key, server.clone());
                    server
                };

                // Create file action
                let mut file_action = FileAction::new(filepath.to_string());
                file_action.single_server = Some(lsp_server);
                file_action.single_server_info = Some(config);

                let file_action = Arc::new(file_action);
                self.file_actions.insert(filepath.to_string(), file_action);

                Ok(true)
            }
            None => {
                tracing::warn!("No LSP server found for: {}", filepath);
                Ok(false)
            }
        }
    }

    /// Close a file.
    pub async fn close_file(&self, filepath: &str) -> Result<()> {
        if let Some((_, file_action)) = self.file_actions.remove(filepath) {
            for server in file_action.get_lsp_servers() {
                let uri = lsp_server::server::path_to_uri(Path::new(filepath));
                server.send_did_close(&uri)?;
            }
        }
        Ok(())
    }

    /// Find a server config for the given file extension.
    fn find_server_config(&self, extension: &str) -> Result<Option<ServerConfig>> {
        // Load all configs and find one matching the extension
        let configs = config::load_all_server_configs(&self.langserver_dir)?;

        // Map common extensions to language IDs
        let lang_id = match extension {
            "py" => "python",
            "rs" => "rust",
            "js" | "jsx" => "javascript",
            "ts" | "tsx" => "typescript",
            "c" | "h" => "c",
            "cpp" | "cc" | "cxx" | "hpp" => "c++",
            "go" => "go",
            "java" => "java",
            "el" => "emacs-lisp",
            "lua" => "lua",
            "rb" => "ruby",
            "sh" | "bash" => "bash",
            "css" => "css",
            "html" => "html",
            "json" => "json",
            "yaml" | "yml" => "yaml",
            "toml" => "toml",
            "zig" => "zig",
            other => other,
        };

        for config in configs.values() {
            if config.language_id == lang_id {
                return Ok(Some(config.clone()));
            }
        }
        Ok(None)
    }

    /// Create an LSP server from config.
    async fn create_lsp_server(
        &self,
        config: &ServerConfig,
        project_path: &Path,
        enable_diagnostics: bool,
    ) -> Result<Arc<LspServer>> {
        let config_json = serde_json::to_value(config)?;

        let on_notification: Arc<lsp_server::server::NotificationCallback> =
            Arc::new(Box::new(|method, params| {
                tracing::debug!("LSP notification: {} {:?}", method, params);
                // TODO: route notifications to file actions (diagnostics, etc.)
            }));

        let on_server_request: Arc<lsp_server::server::ServerRequestCallback> =
            Arc::new(Box::new(|id, method, params| {
                tracing::debug!("LSP server request: {} (id={})", method, id);
                // TODO: handle workspace/configuration, workspace/applyEdit
            }));

        let server = LspServer::spawn(
            config.name.clone(),
            project_path.to_path_buf(),
            config_json,
            enable_diagnostics,
            on_notification,
            on_server_request,
        )
        .await?;

        // Send initialize
        server.send_initialize(None).await?;

        Ok(server)
    }

    /// Dispatch an EPC method call to the appropriate handler.
    ///
    /// Mirrors Python's build_file_action_function pattern.
    pub async fn dispatch(
        &self,
        method: &str,
        args: Vec<SexpValue>,
    ) -> Result<SexpValue> {
        // Convert sexp args to JSON for handler processing
        let json_args: Vec<Value> = args.iter().map(|a| a.to_json()).collect();

        // First arg is always filepath for file action methods
        let filepath = json_args
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if filepath.is_empty() {
            return Ok(SexpValue::Nil);
        }

        // Ensure file is open
        if !self.file_actions.contains_key(&filepath) {
            self.open_file(&filepath).await?;
        }

        let file_action = match self.file_actions.get(&filepath) {
            Some(fa) => fa.clone(),
            None => return Ok(SexpValue::Nil),
        };

        // Look up handler
        if let Some(handler) = self.handlers.get(method) {
            let servers = file_action.get_lsp_servers();
            if servers.is_empty() {
                return Ok(SexpValue::Nil);
            }

            // For each server, dispatch the request
            for server in &servers {
                let flags = server.capability_flags.read().await;

                let request_ctx = RequestContext {
                    args: json_args[1..].to_vec(), // skip filepath
                    server_name: server.server_name.clone(),
                    trigger_characters: flags.completion_trigger_characters.clone(),
                    server_info: server.server_info.clone(),
                };

                let params = handler.process_request(&request_ctx)?;

                // Add textDocument.uri if needed
                let params = if handler.send_document_uri() {
                    let uri = lsp_server::server::path_to_uri(Path::new(&filepath));
                    let mut p = params;
                    if let Value::Object(ref mut map) = p {
                        map.insert(
                            "textDocument".to_string(),
                            json!({"uri": uri}),
                        );
                    }
                    p
                } else {
                    params
                };

                // Send to LSP server
                let response = server.send_request(handler.method(), params).await?;

                // Build response context
                let server_names = file_action.get_server_names();
                let response_ctx = ResponseContext {
                    filepath: filepath.clone(),
                    host: String::new(),
                    server_name: server.server_name.clone(),
                    trigger_characters: flags.completion_trigger_characters.clone(),
                    server_names,
                    eval_in_emacs: Box::new(|_method, _args| {
                        // TODO: wire to real Emacs EPC client
                        tracing::debug!("eval_in_emacs: {} ({} args)", _method, _args.len());
                    }),
                    message_emacs: Box::new(|msg| {
                        tracing::info!("message_emacs: {}", msg);
                    }),
                };

                handler.process_response(&response_ctx, response).await?;
            }
        }

        Ok(SexpValue::Nil)
    }
}

/// Find the project root for a file (simplified version).
///
/// Looks for common project root markers walking up from the file.
fn find_project_path(filepath: &Path) -> PathBuf {
    let dir = if filepath.is_file() {
        filepath.parent().unwrap_or(filepath)
    } else {
        filepath
    };

    let markers = [
        ".git",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "setup.py",
        "go.mod",
        "CMakeLists.txt",
        ".project",
        "pom.xml",
        "build.gradle",
    ];

    let mut current = dir;
    loop {
        for marker in &markers {
            if current.join(marker).exists() {
                return current.to_path_buf();
            }
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent,
            _ => break,
        }
    }

    // Fallback to the file's directory
    dir.to_path_buf()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn file_action_new() {
        let fa = FileAction::new("/tmp/test.py".to_string());
        assert_eq!(fa.filepath, "/tmp/test.py");
        assert!(fa.get_lsp_servers().is_empty());
        assert!(fa.get_server_names().is_empty());
    }

    #[test]
    fn find_project_path_fallback() {
        // Non-existent path: is_file() returns false, so path is treated as-is
        // and no markers are found → returns the path itself
        let path = PathBuf::from("/nonexistent/dir/file.py");
        let project = find_project_path(&path);
        // Since file doesn't exist, is_file() is false, so it walks up from the path itself
        assert_eq!(project, PathBuf::from("/nonexistent/dir/file.py"));
    }

    #[test]
    fn bridge_new() {
        let bridge = LspBridge::new(PathBuf::from("/tmp"));
        assert!(bridge.file_actions.is_empty());
        assert!(bridge.lsp_servers.is_empty());
        assert!(bridge.handlers.contains_key("completion"));
        assert!(bridge.handlers.contains_key("hover"));
    }

    #[test]
    fn bridge_handler_registry() {
        let bridge = LspBridge::new(PathBuf::from("/tmp"));
        // Verify all expected handlers are registered
        let expected = [
            "completion", "hover", "find_define", "find_type_define",
            "find_implementation", "find_references", "code_action",
        ];
        for name in &expected {
            assert!(
                bridge.handlers.contains_key(name),
                "missing handler: {}",
                name
            );
        }
    }
}
