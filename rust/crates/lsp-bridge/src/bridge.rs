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

    /// Get the servers that should handle a specific method.
    ///
    /// Mirrors Python's FileAction.get_method_server_names + call routing.
    /// For single-server: always returns the single server.
    /// For multi-server: looks up the method in multi_servers_info config.
    pub fn get_servers_for_method(&self, method: &str) -> Vec<Arc<LspServer>> {
        if let Some(ref server) = self.single_server {
            return vec![server.clone()];
        }

        let (multi_info, multi_servers) = match (&self.multi_servers_info, &self.multi_servers) {
            (Some(info), Some(servers)) => (info, servers),
            _ => return vec![],
        };

        let default_name = &multi_info.default;

        // Which server names handle this method?
        let server_names: Vec<String> = match method {
            // These methods can go to multiple servers
            "completion" | "completion_item_resolve" | "diagnostics" | "code_action" => {
                let target = match method {
                    "completion" | "completion_item_resolve" => &multi_info.completion,
                    "diagnostics" => &multi_info.diagnostics,
                    "code_action" => &multi_info.code_action,
                    _ => unreachable!(),
                };
                match target {
                    Some(t) => t.names().iter().map(|s| s.to_string()).collect(),
                    None => vec![default_name.clone()],
                }
            }
            // These methods go to a specific server or default
            "formatting" => {
                match &multi_info.formatting {
                    Some(t) => t.names().iter().map(|s| s.to_string()).collect(),
                    None => vec![default_name.clone()],
                }
            }
            "execute_command" => {
                match &multi_info.execute_command {
                    Some(t) => t.names().iter().map(|s| s.to_string()).collect(),
                    None => vec![default_name.clone()],
                }
            }
            // All other methods → default server only
            _ => vec![default_name.clone()],
        };

        server_names.iter()
            .filter_map(|name| multi_servers.get(name).cloned())
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

        // Ask Emacs for the server config — try multi-server first, then single.
        // This mirrors Python's open_file() which checks get-multi-lang-server first.
        let mut multi_server_name = None;
        let mut single_server_name = None;

        if let Some(emacs) = &self.emacs {
            // Try multi-lang-server first
            match emacs.get_emacs_func_result(
                "get-multi-lang-server",
                vec![
                    SexpValue::String(project_path.to_string_lossy().to_string()),
                    SexpValue::String(filepath.to_string()),
                ],
            ).await {
                Ok(SexpValue::String(name)) if !name.is_empty() => {
                    multi_server_name = Some(name);
                }
                _ => {}
            }

            // Try single-lang-server
            if multi_server_name.is_none() {
                match emacs.get_emacs_func_result(
                    "get-single-lang-server",
                    vec![
                        SexpValue::String(project_path.to_string_lossy().to_string()),
                        SexpValue::String(filepath.to_string()),
                    ],
                ).await {
                    Ok(SexpValue::String(name)) if !name.is_empty() => {
                        single_server_name = Some(name);
                    }
                    _ => {}
                }
            }
        }

        // Multi-server: spawn ALL servers in the config
        if let Some(multi_name) = &multi_server_name {
            let multi_path = self.multiserver_dir.join(format!("{}.json", multi_name));
            if let Ok(multi_config) = config::load_multi_server_config(&multi_path) {
                // Collect unique server names from all methods
                let mut server_names: Vec<String> = Vec::new();
                let multi_json: serde_json::Value = serde_json::to_value(&multi_config).unwrap_or_default();
                for (_key, val) in multi_json.as_object().unwrap_or(&serde_json::Map::new()) {
                    match val {
                        Value::String(s) => {
                            if !server_names.contains(s) { server_names.push(s.clone()); }
                        }
                        Value::Array(arr) => {
                            for item in arr {
                                if let Some(s) = item.as_str() {
                                    if !server_names.contains(&s.to_string()) {
                                        server_names.push(s.to_string());
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }

                tracing::info!("Multi-server '{}': spawning servers {:?}", multi_name, server_names);

                let mut multi_servers = HashMap::new();
                let diagnostics_servers: Vec<String> = multi_config.diagnostics.as_ref()
                    .map(|t| t.names().iter().map(|s| s.to_string()).collect())
                    .unwrap_or_default();

                for sname in &server_names {
                    if let Some(cfg) = self.load_server_config(sname)? {
                        if let Some(cmd) = cfg.command.first() {
                            if which::which(cmd).is_err() {
                                tracing::warn!("Multi-server '{}': command '{}' not found, skipping", sname, cmd);
                                continue;
                            }
                        }
                        let server_key = format!("{}#{}", cfg.name, project_path.display());
                        let enable_diag = diagnostics_servers.contains(sname);
                        let server = if let Some(s) = self.lsp_servers.get(&server_key) {
                            s.clone()
                        } else {
                            let s = self.create_lsp_server(&cfg, &project_path, enable_diag).await?;
                            self.lsp_servers.insert(server_key, s.clone());
                            s
                        };

                        // Send didOpen for each server
                        let ext = filepath_path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                        let language_id = self.get_language_id(filepath, &cfg, &ext).await;
                        let uri = lsp_server::server::path_to_uri(filepath_path);
                        let text = std::fs::read_to_string(filepath).unwrap_or_default();
                        let _ = server.send_did_open(&uri, &language_id, 0, &text);
                        tracing::info!("didOpen {} on '{}' as languageId={}", filepath, sname, language_id);

                        multi_servers.insert(cfg.name.clone(), server);
                    }
                }

                if !multi_servers.is_empty() {
                    if let Some(emacs) = &self.emacs {
                        let _ = emacs.message_emacs(&format!(
                            "Active {} '{}', enjoy hacking!",
                            if project_path.is_dir() { "project" } else { "file" },
                            project_path.file_name().unwrap_or_default().to_string_lossy()
                        )).await;
                    }

                    let mut file_action = FileAction::new(filepath.to_string());
                    file_action.multi_servers = Some(multi_servers);
                    file_action.multi_servers_info = Some(multi_config);
                    self.file_actions.insert(filepath.to_string(), Arc::new(file_action));
                    return Ok(true);
                }
                // If no multi-server succeeded, fall through to single
            }
        }

        // Single-server path
        let server_name = single_server_name
            .or_else(|| {
                let ext = filepath_path.extension().and_then(|e| e.to_str()).unwrap_or("");
                self.find_server_config_by_extension(ext).map(|c| c.name.clone())
            });

        let server_name = match server_name {
            Some(name) => name,
            None => {
                tracing::warn!("No LSP server found for: {}", filepath);
                return Ok(false);
            }
        };

        tracing::info!("Opening {} with server '{}' in project {}", filepath, server_name, project_path.display());

        let config = match self.load_server_config(&server_name)? {
            Some(c) => c,
            None => {
                tracing::error!("Server config not found: {}", server_name);
                return Ok(false);
            }
        };

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
        let lsp_server = if let Some(server) = self.lsp_servers.get(&server_key) {
            server.clone()
        } else {
            let server = self.create_lsp_server(&config, &project_path, true).await?;
            self.lsp_servers.insert(server_key, server.clone());
            if let Some(emacs) = &self.emacs {
                let _ = emacs.message_emacs(&format!(
                    "Active {} '{}', enjoy hacking!",
                    if project_path.is_dir() { "project" } else { "file" },
                    project_path.file_name().unwrap_or_default().to_string_lossy()
                )).await;
            }
            server
        };

        let ext = filepath_path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        let language_id = self.get_language_id(filepath, &config, &ext).await;
        tracing::info!("didOpen {} as languageId={}", filepath, language_id);

        let uri = lsp_server::server::path_to_uri(filepath_path);
        let text = std::fs::read_to_string(filepath).unwrap_or_default();
        lsp_server.send_did_open(&uri, &language_id, 0, &text)?;

        let mut file_action = FileAction::new(filepath.to_string());
        file_action.single_server = Some(lsp_server);
        file_action.single_server_info = Some(config);
        self.file_actions.insert(filepath.to_string(), Arc::new(file_action));

        Ok(true)
    }

    /// Determine the language ID for a file.
    ///
    /// Mirrors Python's LspServer.get_language_id():
    /// 1. Ask Emacs via get-language-id (user can customize)
    /// 2. Check config's languageIds map (extension → language ID)
    /// 3. Use config's languageId field
    /// 4. Fallback to file extension
    async fn get_language_id(&self, filepath: &str, config: &ServerConfig, ext: &str) -> String {
        // 1. Ask Emacs
        if let Some(emacs) = &self.emacs {
            let server_name = config.name.split('#').last().unwrap_or(&config.name);
            let project_path = find_project_path(Path::new(filepath));
            if let Ok(SexpValue::String(lang_id)) = emacs.get_emacs_func_result(
                "get-language-id",
                vec![
                    SexpValue::String(project_path.to_string_lossy().to_string()),
                    SexpValue::String(filepath.to_string()),
                    SexpValue::String(server_name.to_string()),
                    SexpValue::String(ext.to_string()),
                ],
            ).await {
                if !lang_id.is_empty() {
                    return lang_id;
                }
            }
        }

        // 2. Check languageIds map (e.g., {"tsx": "typescriptreact"})
        if let Some(lang_id) = config.language_ids.get(ext) {
            return lang_id.clone();
        }

        // 3. Use config's languageId field
        if !config.language_id.is_empty() {
            return config.language_id.clone();
        }

        // 4. Fallback to extension
        ext.to_string()
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
        let server_name = config.name.clone();

        // Route LSP notifications (especially diagnostics) to Emacs
        let emacs_for_notif = self.emacs.clone();
        let on_notification: Arc<lsp_server::server::NotificationCallback> =
            Arc::new(Box::new(move |method, params| {
                if method == "textDocument/publishDiagnostics" {
                    // Forward diagnostics to Emacs
                    if let Some(emacs) = &emacs_for_notif {
                        let emacs = emacs.clone();
                        let server_name = server_name.clone();
                        let uri = params.get("uri").and_then(|u| u.as_str()).unwrap_or("").to_string();
                        let filepath = lsp_server::server::uri_to_path(&uri);
                        let diagnostics = params.get("diagnostics").cloned().unwrap_or(json!([]));
                        let diag_count = diagnostics.as_array().map(|a| a.len()).unwrap_or(0);

                        tokio::spawn(async move {
                            let args = vec![
                                epc::types::EvalArg::String(filepath),
                                epc::types::EvalArg::String(String::new()), // host
                                epc::types::EvalArg::Raw(SexpValue::from_json(&diagnostics)),
                                epc::types::EvalArg::Integer(diag_count as i64),
                            ];
                            if let Err(e) = emacs.eval_in_emacs("lsp-bridge-diagnostic--render", &args).await {
                                tracing::error!("Failed to send diagnostics to Emacs: {}", e);
                            }
                        });
                    }
                } else {
                    tracing::debug!("LSP notification: {}", method);
                }
            }));

        let on_server_request: Arc<lsp_server::server::ServerRequestCallback> =
            Arc::new(Box::new(|id, method, _params| {
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

    /// Helper: call eval_in_emacs with a method name and args.
    async fn eval_in_emacs_method(&self, method: &str, args: Vec<Value>) {
        if let Some(emacs) = &self.emacs {
            let eval_args: Vec<epc::types::EvalArg> = args.iter().map(|a| {
                epc::types::EvalArg::Raw(SexpValue::from_json(a))
            }).collect();
            if let Err(e) = emacs.eval_in_emacs(method, &eval_args).await {
                tracing::error!("eval_in_emacs '{}' error: {}", method, e);
            }
        }
    }

    /// Handle completion response: build candidates and send to Emacs.
    async fn handle_completion_response(
        &self,
        filepath: &str,
        server_name: &str,
        flags: &lsp_server::capabilities::ServerCapabilityFlags,
        prefix: &str,
        position: &Value,
        response: Value,
    ) {
        let items = if let Some(items) = response.get("items").and_then(|i| i.as_array()) {
            items.clone()
        } else if response.is_array() {
            response.as_array().cloned().unwrap_or_default()
        } else {
            return;
        };

        let mut candidates: Vec<Value> = Vec::new();
        for item in &items {
            let kind_num = item.get("kind").and_then(|k| k.as_u64()).unwrap_or(0);
            let kind = handlers::kind_name(kind_num).to_lowercase();
            let label = item.get("label").and_then(|l| l.as_str()).unwrap_or("");
            let detail = item.get("detail").and_then(|d| d.as_str()).unwrap_or("");
            let annotation = if !kind.is_empty() { &kind } else { detail };
            let key = format!("{}_{}", label, detail);

            candidates.push(json!({
                "key": key,
                "icon": annotation,
                "label": label,
                "displayLabel": label,
                "deprecated": item.get("tags").and_then(|t| t.as_array())
                    .map(|tags| tags.iter().any(|t| t.as_u64() == Some(1)))
                    .unwrap_or(false),
                "insertText": item.get("insertText"),
                "insertTextFormat": item.get("insertTextFormat").cloned().unwrap_or(Value::String(String::new())),
                "textEdit": item.get("textEdit"),
                "score": item.get("score").and_then(|s| s.as_f64()).unwrap_or(1000.0),
                "sortText": item.get("sortText").and_then(|s| s.as_str()).unwrap_or(""),
                "filterText": item.get("filterText"),
                "server": server_name,
                "backend": "lsp"
            }));
        }

        self.eval_in_emacs_method("lsp-bridge-completion--record-items", vec![
            Value::String(filepath.to_string()),
            Value::String(String::new()), // host
            Value::Array(candidates),
            position.clone(),
            Value::String(server_name.to_string()),
            Value::Array(flags.completion_trigger_characters.iter()
                .map(|s| Value::String(s.clone())).collect()),
            Value::Array(vec![Value::String(server_name.to_string())]),
        ]).await;
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

        // Handle FileAction internal methods (not LSP handlers).
        // These mirror Python's FileAction.change_file, save_file, etc.
        match method {
            "change_file" => {
                // Args: [filepath, start, end, range_length, change_text, position, before_char, buffer_name, prefix]
                // Sends textDocument/didChange to LSP servers
                let servers = file_action.get_lsp_servers();
                let version = file_action.version.fetch_add(1, std::sync::atomic::Ordering::SeqCst) as i32;
                for server in &servers {
                    let flags = server.capability_flags.read().await;
                    let sync_kind = flags.text_document_sync_kind;
                    let uri = lsp_server::server::path_to_uri(Path::new(&filepath));

                    if sync_kind == lsp_types::TextDocumentSyncKind::NONE {
                        continue;
                    } else if sync_kind == lsp_types::TextDocumentSyncKind::FULL {
                        // Full sync: send entire file content
                        let text = std::fs::read_to_string(&filepath).unwrap_or_default();
                        let _ = server.send_did_change_full(&uri, version, &text);
                    } else {
                        // Incremental sync
                        let start = json_args.get(1).cloned().unwrap_or(Value::Null);
                        let end = json_args.get(2).cloned().unwrap_or(Value::Null);
                        let range_length = json_args.get(3).and_then(|v| v.as_u64()).unwrap_or(0);
                        let change_text = json_args.get(4).and_then(|v| v.as_str()).unwrap_or("");
                        let _ = server.send_did_change_incremental(
                            &uri, version, start, end, range_length, change_text,
                        );
                    }
                }
                return Ok(());
            }
            "update_file" => {
                // Full file resync — send entire buffer content
                let servers = file_action.get_lsp_servers();
                let version = file_action.version.fetch_add(1, std::sync::atomic::Ordering::SeqCst) as i32;
                // Args: [filepath, buffer_name, ...]
                // For now, read file from disk
                let text = std::fs::read_to_string(&filepath).unwrap_or_default();
                let uri = lsp_server::server::path_to_uri(Path::new(&filepath));
                for server in &servers {
                    let _ = server.send_did_change_full(&uri, version, &text);
                }
                return Ok(());
            }
            "save_file" => {
                // Send textDocument/didSave
                let uri = lsp_server::server::path_to_uri(Path::new(&filepath));
                for server in file_action.get_lsp_servers() {
                    let flags = server.capability_flags.read().await;
                    if flags.save_provider {
                        let text = if flags.save_include_text {
                            Some(std::fs::read_to_string(&filepath).unwrap_or_default())
                        } else {
                            None
                        };
                        let _ = server.send_did_save(&uri, text.as_deref());
                    }
                }
                return Ok(());
            }
            "change_cursor" => {
                // Just record cursor time for staleness checks
                file_action.last_change_cursor_time.store(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    std::sync::atomic::Ordering::SeqCst,
                );
                return Ok(());
            }
            "list_diagnostics" => {
                return Ok(());
            }
            "try_formatting" => {
                // Python: try_formatting(start, end, *args)
                // If start == end → formatting(tab_size)
                // Else → rangeFormatting(start, end, tab_size)
                let start = json_args.get(1).cloned().unwrap_or(Value::Null);
                let end = json_args.get(2).cloned().unwrap_or(Value::Null);
                let tab_size = json_args.get(3).cloned().unwrap_or(json!(4));

                for server in file_action.get_servers_for_method("formatting") {
                    let flags = server.capability_flags.read().await;
                    let uri = lsp_server::server::path_to_uri(Path::new(&filepath));

                    let (lsp_method, params) = if start == end {
                        if !flags.code_format_provider { continue; }
                        ("textDocument/formatting", json!({
                            "textDocument": {"uri": uri},
                            "options": {"tabSize": tab_size, "insertSpaces": true,
                                "trimTrailingWhitespace": true, "insertFinalNewline": false, "trimFinalNewlines": true}
                        }))
                    } else {
                        if !flags.range_format_provider { continue; }
                        ("textDocument/rangeFormatting", json!({
                            "textDocument": {"uri": uri},
                            "range": {"start": start, "end": end},
                            "options": {"tabSize": tab_size, "insertSpaces": true,
                                "trimTrailingWhitespace": true, "insertFinalNewline": false, "trimFinalNewlines": true}
                        }))
                    };

                    match tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        server.send_request(lsp_method, params),
                    ).await {
                        Ok(Ok(resp)) if !resp.is_null() && resp.as_array().is_some_and(|a| !a.is_empty()) => {
                            self.eval_in_emacs_method("lsp-bridge-format--update",
                                vec![Value::String(filepath.clone()), resp]).await;
                        }
                        _ => {}
                    }
                }
                return Ok(());
            }
            "try_code_action" | "code_action" => {
                // Python: try_code_action(range_start, range_end, action_kind)
                // Preprocesses: adds server_name and diagnostics before calling handler
                let range_start = json_args.get(1).cloned().unwrap_or(Value::Null);
                let range_end = json_args.get(2).cloned().unwrap_or(Value::Null);
                let action_kind = json_args.get(3).cloned().unwrap_or(Value::Null);

                for server in file_action.get_servers_for_method("code_action") {
                    let flags = server.capability_flags.read().await;
                    if !flags.code_action_provider { continue; }

                    let uri = lsp_server::server::path_to_uri(Path::new(&filepath));
                    let mut context = json!({"diagnostics": []});
                    if let Some(kind_str) = action_kind.as_str() {
                        context["only"] = json!([kind_str]);
                    }

                    let params = json!({
                        "textDocument": {"uri": uri},
                        "range": {"start": range_start, "end": range_end},
                        "context": context
                    });

                    match tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        server.send_request("textDocument/codeAction", params),
                    ).await {
                        Ok(Ok(response)) if !response.is_null() && !response.get("error").is_some() => {
                            if let Some(actions) = response.as_array() {
                                if !actions.is_empty() {
                                    self.eval_in_emacs_method("lsp-bridge-code-action--fix",
                                        vec![response, action_kind.clone()]).await;
                                }
                            }
                        }
                        Ok(Err(e)) => tracing::warn!("code_action error: {}", e),
                        Err(_) => tracing::warn!("code_action timed out"),
                        _ => {}
                    }
                }
                return Ok(());
            }
            "try_completion" | "completion" => {
                // Python: try_completion(position, before_char, prefix, version)
                // Sends textDocument/completion with proper trigger context
                let position = json_args.get(1).cloned().unwrap_or(Value::Null);
                let before_char = json_args.get(2).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let prefix = json_args.get(3).and_then(|v| v.as_str()).unwrap_or("").to_string();

                for server in file_action.get_servers_for_method("completion") {
                    let flags = server.capability_flags.read().await;
                    let uri = lsp_server::server::path_to_uri(Path::new(&filepath));

                    let context = if flags.completion_trigger_characters.iter().any(|tc| tc == &before_char) {
                        json!({"triggerKind": 2, "triggerCharacter": before_char})
                    } else {
                        json!({"triggerKind": 1})
                    };

                    let params = json!({
                        "textDocument": {"uri": uri},
                        "position": position,
                        "context": context
                    });

                    match tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        server.send_request("textDocument/completion", params),
                    ).await {
                        Ok(Ok(response)) if !response.is_null() && !response.get("error").is_some() => {
                            self.handle_completion_response(
                                &filepath, &server.server_name, &flags, &prefix, &position, response,
                            ).await;
                        }
                        Ok(Err(e)) => tracing::warn!("completion error: {}", e),
                        Err(_) => tracing::warn!("completion timed out"),
                        _ => {}
                    }
                }
                return Ok(());
            }
            "rename_file" => {
                // Python: FileAction.rename_file(old_filepath, new_filepath)
                // Args: [old_filepath, new_filepath]
                let old_path = json_args.get(0).and_then(|v| v.as_str()).unwrap_or("");
                let new_path = json_args.get(1).and_then(|v| v.as_str()).unwrap_or("");
                if let Some(server) = file_action.get_lsp_servers().first() {
                    let _ = server.send_notification("workspace/didRenameFiles", json!({
                        "files": [{"oldUri": lsp_server::server::path_to_uri(Path::new(old_path)),
                                   "newUri": lsp_server::server::path_to_uri(Path::new(new_path))}]
                    }));
                }
                return Ok(());
            }
            "fetch_completion_item_info" => {
                // Python: fetch_completion_item_info(filepath, item_key, server_name)
                // NOT dispatched through FileAction — called directly on LspBridge
                // For now, just log — full completion_item_resolve needs stored items
                let item_key = json_args.get(1).and_then(|v| v.as_str()).unwrap_or("");
                let server_name = json_args.get(2).and_then(|v| v.as_str()).unwrap_or("");
                tracing::debug!("fetch_completion_item_info: key={} server={}", item_key, server_name);
                // TODO: implement completion item resolve with stored items
                return Ok(());
            }
            _ => {} // Fall through to handler dispatch
        }

        // Generic handler dispatch — mirrors Python's FileAction.call() → send_request()
        let handler = match self.handlers.get(method) {
            Some(h) => h,
            None => {
                tracing::debug!("no handler for method: {}", method);
                return Ok(());
            }
        };

        let servers = file_action.get_servers_for_method(method);
        if servers.is_empty() {
            return Ok(());
        }

        for server in &servers {
            let flags = server.capability_flags.read().await;

            // Provider capability check — mirrors Python's send_request()
            // Python: if hasattr(handler, "provider"), check getattr(server, handler.provider)
            // We check known provider mappings here
            let skip = match method {
                "formatting" | "rangeFormatting" => !flags.code_format_provider && !flags.range_format_provider,
                "signature_help" => !flags.signature_help_provider,
                "document_highlight" => !flags.document_highlight_provider,
                "inlay_hint" => !flags.inlay_hint_provider,
                "semantic_tokens" => !flags.semantic_tokens_provider,
                "workspace_symbol" => !flags.workspace_symbol_provider,
                "prepare_rename" => !flags.rename_prepare_provider,
                "diagnostic" => !flags.diagnostic_provider,
                _ => false, // No provider check for other handlers
            };
            if skip {
                tracing::debug!("handler '{}': server '{}' doesn't support this, skipping", method, server.server_name);
                continue;
            }

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
