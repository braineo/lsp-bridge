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
use epc::sexp::SexpValue;

use lsp_server::server::LspServer;

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

    pub fn get_lsp_servers(&self) -> Vec<Arc<LspServer>> {
        if let Some(ref server) = self.single_server {
            vec![server.clone()]
        } else if let Some(ref servers) = self.multi_servers {
            servers.values().cloned().collect()
        } else {
            vec![]
        }
    }

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

    pub fn set_emacs(&mut self, client: Arc<EpcClient>) {
        self.emacs = Some(client);
    }

    pub fn emacs(&self) -> &Arc<EpcClient> {
        self.emacs.as_ref().expect("Emacs client not initialized")
    }

    /// Open a file — query Emacs for server config, create LSP server, create FileAction.
    ///
    /// Mirrors Python's open_file() in lsp_bridge.py:
    /// 1. get_project_path from Emacs
    /// 2. get-multi-lang-server or get-single-lang-server from Emacs
    /// 3. Load config JSON
    /// 4. Spawn or reuse LSP server
    /// 5. Create FileAction
    pub async fn open_file(&self, filepath: &str) -> Result<bool> {
        if self.file_actions.contains_key(filepath) {
            return Ok(true);
        }

        let filepath_path = Path::new(filepath);

        // Ask Emacs for the project path
        let project_path = if let Some(emacs) = &self.emacs {
            match emacs.get_emacs_func_result(
                "get-project-path",
                vec![SexpValue::String(filepath.to_string())],
            ).await {
                Ok(SexpValue::String(p)) => PathBuf::from(p),
                _ => find_project_path(filepath_path),
            }
        } else {
            find_project_path(filepath_path)
        };

        // Ask Emacs which single-lang-server to use
        let server_name = if let Some(emacs) = &self.emacs {
            match emacs.get_emacs_func_result(
                "get-single-lang-server",
                vec![
                    SexpValue::String(project_path.to_string_lossy().to_string()),
                    SexpValue::String(filepath.to_string()),
                ],
            ).await {
                Ok(SexpValue::String(name)) => Some(name),
                _ => None,
            }
        } else {
            None
        };

        let server_name = match server_name {
            Some(name) => name,
            None => {
                tracing::warn!("Emacs returned no server for: {}", filepath);
                // Fallback: try to find by extension
                let ext = filepath_path.extension().and_then(|e| e.to_str()).unwrap_or("");
                match self.find_server_config_by_extension(ext) {
                    Some(c) => c.name.clone(),
                    None => {
                        tracing::warn!("No LSP server found for: {}", filepath);
                        return Ok(false);
                    }
                }
            }
        };

        tracing::info!("Opening {} with server '{}' in project {}", filepath, server_name, project_path.display());

        // Load server config
        let config = self.load_server_config(&server_name)?;
        let config = match config {
            Some(c) => c,
            None => {
                tracing::error!("Server config not found: {}", server_name);
                return Ok(false);
            }
        };

        // Check command exists
        if let Some(cmd) = config.command.first() {
            if which::which(cmd).is_err() {
                tracing::error!("LSP server command not found: {}", cmd);
                if let Some(emacs) = &self.emacs {
                    let _ = emacs.message_emacs(&format!("LSP server '{}' not found", cmd)).await;
                }
                return Ok(false);
            }
        }

        let server_key = format!("{}#{}", config.name, project_path.display());

        // Reuse existing server or create new one
        let lsp_server = if let Some(server) = self.lsp_servers.get(&server_key) {
            server.clone()
        } else {
            let server = self.create_lsp_server(&config, &project_path, true).await?;
            self.lsp_servers.insert(server_key, server.clone());

            // Notify user
            if let Some(emacs) = &self.emacs {
                let _ = emacs.message_emacs(&format!(
                    "Active {} '{}', enjoy hacking!",
                    if project_path.is_dir() { "project" } else { "file" },
                    project_path.file_name().unwrap_or_default().to_string_lossy()
                )).await;
            }

            server
        };

        // Send didOpen
        let uri = lsp_server::server::path_to_uri(filepath_path);
        let language_id = if !config.language_id.is_empty() {
            config.language_id.clone()
        } else {
            // Try languageIds map
            let ext = filepath_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            config.language_ids.get(ext).cloned().unwrap_or_else(|| ext.to_string())
        };
        let text = std::fs::read_to_string(filepath).unwrap_or_default();
        lsp_server.send_did_open(&uri, &language_id, 0, &text)?;

        // Create file action
        let mut file_action = FileAction::new(filepath.to_string());
        file_action.single_server = Some(lsp_server);
        file_action.single_server_info = Some(config);
        self.file_actions.insert(filepath.to_string(), Arc::new(file_action));

        Ok(true)
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

    /// Load a server config by name from langserver/*.json.
    fn load_server_config(&self, name: &str) -> Result<Option<ServerConfig>> {
        let path = self.langserver_dir.join(format!("{}.json", name));
        if path.exists() {
            Ok(Some(config::load_server_config(&path)?))
        } else {
            // Try loading all and searching by name
            let configs = config::load_all_server_configs(&self.langserver_dir)?;
            Ok(configs.into_values().find(|c| c.name == name))
        }
    }

    /// Fallback: find a server config by file extension.
    fn find_server_config_by_extension(&self, extension: &str) -> Option<ServerConfig> {
        let configs = config::load_all_server_configs(&self.langserver_dir).ok()?;
        let lang_id = ext_to_language_id(extension);

        // Prefer configs where the command actually exists
        configs.into_values()
            .filter(|c| c.language_id == lang_id || c.language_ids.contains_key(extension))
            .find(|c| {
                c.command.first()
                    .map(|cmd| which::which(cmd).is_ok())
                    .unwrap_or(false)
            })
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
                tracing::debug!("LSP notification: {}", method);
                // TODO: route diagnostics to file actions
            }));

        let on_server_request: Arc<lsp_server::server::ServerRequestCallback> =
            Arc::new(Box::new(|id, method, params| {
                tracing::debug!("LSP server request: {} (id={})", method, id);
            }));

        let server = LspServer::spawn(
            config.name.clone(),
            project_path.to_path_buf(),
            config_json,
            enable_diagnostics,
            on_notification,
            on_server_request,
        ).await?;

        server.send_initialize(None).await?;

        // Give server time to initialize
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        Ok(server)
    }

    /// Dispatch an EPC method call to the appropriate handler.
    ///
    /// Never propagates errors — logs them instead. This prevents a single
    /// failed LSP request from killing the EPC connection (which would cause
    /// "client exited without proper shutdown sequence" in LSP servers).
    pub async fn dispatch(
        &self,
        method: &str,
        args: Vec<SexpValue>,
    ) -> Result<SexpValue> {
        if let Err(e) = self.dispatch_inner(method, args).await {
            tracing::error!("dispatch '{}' error: {}", method, e);
        }
        Ok(SexpValue::Nil)
    }

    async fn dispatch_inner(
        &self,
        method: &str,
        args: Vec<SexpValue>,
    ) -> Result<()> {
        let json_args: Vec<Value> = args.iter().map(|a| a.to_json()).collect();

        let filepath = json_args
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if filepath.is_empty() {
            return Ok(());
        }

        // Ensure file is open
        if !self.file_actions.contains_key(&filepath) {
            if let Err(e) = self.open_file(&filepath).await {
                tracing::error!("open_file for dispatch '{}': {}", method, e);
                return Ok(());
            }
        }

        let file_action = match self.file_actions.get(&filepath) {
            Some(fa) => fa.clone(),
            None => return Ok(()),
        };

        let handler = match self.handlers.get(method) {
            Some(h) => h,
            None => {
                tracing::debug!("no handler for method: {}", method);
                return Ok(());
            }
        };

        let servers = file_action.get_lsp_servers();
        if servers.is_empty() {
            return Ok(());
        }

        for server in &servers {
            let flags = server.capability_flags.read().await;

            let request_ctx = RequestContext {
                args: json_args[1..].to_vec(),
                server_name: server.server_name.clone(),
                trigger_characters: flags.completion_trigger_characters.clone(),
                server_info: server.server_info.clone(),
            };

            let params = match handler.process_request(&request_ctx) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("handler '{}' process_request error: {}", method, e);
                    continue;
                }
            };

            let params = if handler.send_document_uri() {
                let uri = lsp_server::server::path_to_uri(Path::new(&filepath));
                let mut p = params;
                if let Value::Object(ref mut map) = p {
                    map.insert("textDocument".to_string(), json!({"uri": uri}));
                }
                p
            } else {
                params
            };

            // Send request with timeout to avoid hanging forever
            let response = match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                server.send_request(handler.method(), params),
            ).await {
                Ok(Ok(resp)) => resp,
                Ok(Err(e)) => {
                    tracing::error!("handler '{}' LSP request error: {}", method, e);
                    continue;
                }
                Err(_) => {
                    tracing::error!("handler '{}' LSP request timed out (30s)", method);
                    continue;
                }
            };

            // Check for LSP error response
            if response.get("error").is_some() {
                tracing::warn!("handler '{}' LSP error: {}", method,
                    response["error"].get("message").and_then(|m| m.as_str()).unwrap_or("unknown"));
                continue;
            }

            let server_names = file_action.get_server_names();
            let response_ctx = ResponseContext {
                filepath: filepath.clone(),
                host: String::new(),
                server_name: server.server_name.clone(),
                trigger_characters: flags.completion_trigger_characters.clone(),
                server_names,
                eval_in_emacs: if let Some(emacs) = &self.emacs {
                    let emacs = emacs.clone();
                    Box::new(move |method, args| {
                        let emacs = emacs.clone();
                        let method = method.to_string();
                        tokio::spawn(async move {
                            let eval_args: Vec<epc::types::EvalArg> = args.iter().map(|a| {
                                epc::types::EvalArg::Raw(SexpValue::from_json(a))
                            }).collect();
                            if let Err(e) = emacs.eval_in_emacs(&method, &eval_args).await {
                                tracing::error!("eval_in_emacs '{}' error: {}", method, e);
                            }
                        });
                    })
                } else {
                    Box::new(|method, _args| {
                        tracing::debug!("eval_in_emacs (no client): {}", method);
                    })
                },
                message_emacs: if let Some(emacs) = &self.emacs {
                    let emacs = emacs.clone();
                    Box::new(move |msg| {
                        let emacs = emacs.clone();
                        let msg = msg.to_string();
                        tokio::spawn(async move {
                            let _ = emacs.message_emacs(&msg).await;
                        });
                    })
                } else {
                    Box::new(|msg| { tracing::info!("message: {}", msg); })
                },
            };

            if let Err(e) = handler.process_response(&response_ctx, response).await {
                tracing::error!("handler '{}' process_response error: {}", method, e);
            }
        }

        Ok(())
    }
}

/// Map file extension to LSP language ID.
fn ext_to_language_id(ext: &str) -> &str {
    match ext {
        "py" => "python",
        "rs" => "rust",
        "js" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "c++",
        "go" => "go",
        "java" => "java",
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
    }
}

/// Find the project root for a file.
fn find_project_path(filepath: &Path) -> PathBuf {
    let dir = if filepath.is_file() {
        filepath.parent().unwrap_or(filepath)
    } else {
        filepath
    };

    let markers = [
        ".git", "Cargo.toml", "package.json", "pyproject.toml",
        "setup.py", "go.mod", "CMakeLists.txt", "pom.xml", "build.gradle",
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

    dir.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_action_new() {
        let fa = FileAction::new("/tmp/test.py".to_string());
        assert_eq!(fa.filepath, "/tmp/test.py");
        assert!(fa.get_lsp_servers().is_empty());
    }

    #[test]
    fn bridge_new() {
        let bridge = LspBridge::new(PathBuf::from("/tmp"));
        assert!(bridge.handlers.contains_key("completion"));
        assert!(bridge.handlers.contains_key("hover"));
    }

    #[test]
    fn ext_to_lang_id() {
        assert_eq!(ext_to_language_id("py"), "python");
        assert_eq!(ext_to_language_id("rs"), "rust");
        assert_eq!(ext_to_language_id("ts"), "typescript");
        assert_eq!(ext_to_language_id("unknown"), "unknown");
    }

    #[test]
    fn find_project_path_fallback() {
        let path = PathBuf::from("/nonexistent/dir/file.py");
        let project = find_project_path(&path);
        assert_eq!(project, PathBuf::from("/nonexistent/dir/file.py"));
    }
}
