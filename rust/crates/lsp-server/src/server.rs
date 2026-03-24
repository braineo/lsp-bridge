//! LSP server subprocess manager.
//!
//! Spawns an LSP server as a child process, manages the JSON-RPC 2.0
//! communication lifecycle per LSP 3.17 specification:
//!
//! 1. Spawn process with stdio pipes
//! 2. Send `initialize` request (§3.4.1)
//! 3. Receive `InitializeResult` with server capabilities
//! 4. Send `initialized` notification (§3.4.2)
//! 5. Send `workspace/didChangeConfiguration` notification
//! 6. Normal operation: requests, notifications, responses
//! 7. Shutdown: send `shutdown` request, then `exit` notification (§3.4.3, §3.4.4)
//!
//! Reference: https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use lsp_types::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Notify};
use tracing;

use crate::capabilities::{
    self, ServerCapabilityFlags, extract_server_capabilities, merge_capabilities,
};
use crate::transport;
use crate::types::{self, IncomingMessage, RequestIdGenerator};

/// Convert a file path to a file:// URI.
///
/// Per LSP 3.17: DocumentUri uses RFC 3986 URI encoding.
pub fn path_to_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_string_lossy().to_string()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path).to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string())
    };
    // On Unix: file:///path/to/file
    // On Windows: file:///C:/path/to/file
    if abs.starts_with('/') {
        format!("file://{}", abs)
    } else {
        format!("file:///{}", abs.replace('\\', "/"))
    }
}

/// Convert a file:// URI back to a file path.
pub fn uri_to_path(uri: &str) -> String {
    if let Some(path) = uri.strip_prefix("file://") {
        // Handle Windows paths: file:///C:/... → C:/...
        if path.len() > 2 && path.as_bytes()[0] == b'/' && path.as_bytes()[2] == b':' {
            path[1..].to_string()
        } else {
            path.to_string()
        }
    } else {
        uri.to_string()
    }
}

/// Callback type for handling server notifications (e.g., diagnostics).
pub type NotificationCallback = Box<
    dyn Fn(String, serde_json::Value) + Send + Sync,
>;

/// Callback type for handling server requests (e.g., workspace/configuration).
pub type ServerRequestCallback = Box<
    dyn Fn(u64, String, serde_json::Value) + Send + Sync,
>;

/// An LSP server subprocess managed by lsp-bridge.
pub struct LspServer {
    pub server_name: String,
    pub project_path: PathBuf,
    pub server_info: serde_json::Value,

    /// Extracted capability flags for quick checks.
    pub capability_flags: tokio::sync::RwLock<ServerCapabilityFlags>,

    /// Full server capabilities from initialize response.
    pub server_capabilities: tokio::sync::RwLock<Option<ServerCapabilities>>,

    /// Channel to send outgoing JSON-RPC messages to the writer task.
    writer_tx: mpsc::UnboundedSender<OutgoingMessage>,

    /// Pending requests awaiting responses, keyed by request ID.
    pending_requests: Arc<DashMap<u64, oneshot::Sender<serde_json::Value>>>,

    /// Request ID generator.
    id_gen: RequestIdGenerator,

    /// The initialize request ID (to identify the init response).
    initialize_id: u64,

    /// Signaled when the server has been initialized.
    initialized: Arc<Notify>,
}

/// An outgoing message to send to the LSP server.
enum OutgoingMessage {
    /// Goes into the init queue (sent before initialized).
    Init(String),
    /// Goes into the normal queue (sent after initialized).
    Normal(String),
}

impl LspServer {
    /// Spawn an LSP server subprocess and start communication.
    ///
    /// Per LSP 3.17 §3.4: the lifecycle starts with `initialize` request.
    pub async fn spawn(
        server_name: String,
        project_path: PathBuf,
        server_info: serde_json::Value,
        enable_diagnostics: bool,
        on_notification: Arc<NotificationCallback>,
        on_server_request: Arc<ServerRequestCallback>,
    ) -> Result<Arc<Self>> {
        let command = server_info["command"]
            .as_array()
            .context("server config missing 'command' array")?;
        let program = command[0].as_str().context("command[0] must be string")?;
        let args: Vec<&str> = command[1..]
            .iter()
            .filter_map(|v| v.as_str())
            .collect();

        let cwd = if project_path.is_file() {
            project_path.parent().unwrap_or(&project_path).to_path_buf()
        } else {
            project_path.clone()
        };

        let mut child = Command::new(program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .current_dir(&cwd)
            .spawn()
            .with_context(|| format!("failed to spawn LSP server: {}", program))?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;

        let (writer_tx, writer_rx) = mpsc::unbounded_channel();
        let pending_requests: Arc<DashMap<u64, oneshot::Sender<serde_json::Value>>> =
            Arc::new(DashMap::new());
        let initialized = Arc::new(Notify::new());
        let id_gen = RequestIdGenerator::new();
        let initialize_id = id_gen.next();

        let server = Arc::new(Self {
            server_name: server_name.clone(),
            project_path,
            server_info,
            capability_flags: tokio::sync::RwLock::new(ServerCapabilityFlags::default()),
            server_capabilities: tokio::sync::RwLock::new(None),
            writer_tx,
            pending_requests: pending_requests.clone(),
            id_gen,
            initialize_id,
            initialized: initialized.clone(),
        });

        // Spawn writer task
        tokio::spawn(writer_loop(stdin, writer_rx, initialized.clone()));

        // Spawn reader task
        let pending_for_reader = pending_requests.clone();
        let server_for_reader = Arc::downgrade(&server);
        tokio::spawn(reader_loop(
            stdout,
            pending_for_reader,
            on_notification,
            on_server_request,
            server_for_reader,
        ));

        Ok(server)
    }

    /// Send the `initialize` request per LSP 3.17 §3.4.1.
    ///
    /// Must be called after spawn. The response triggers capability
    /// extraction and the `initialized` notification.
    pub async fn send_initialize(
        &self,
        workspace_folders: Option<Vec<WorkspaceFolder>>,
    ) -> Result<()> {
        let config_capabilities = self.server_info.get("capabilities")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let enable_diagnostics = true; // TODO: pass from caller
        let base_caps = capabilities::default_client_capabilities(enable_diagnostics);
        let merged_caps = merge_capabilities(&base_caps, &config_capabilities);

        let init_options = self.server_info.get("initializationOptions")
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        // LSP 3.17 §3.4.1 InitializeParams
        let params = serde_json::json!({
            "processId": std::process::id(),
            "rootPath": self.project_path.to_string_lossy(),
            "rootUri": path_to_uri(&self.project_path),
            "clientInfo": {
                "name": "emacs",
                "version": "lsp-bridge"
            },
            "capabilities": serde_json::to_value(&merged_caps)?,
            "initializationOptions": init_options,
            "workspaceFolders": workspace_folders,
        });

        let json = types::build_request(self.initialize_id, "initialize", params);
        self.writer_tx.send(OutgoingMessage::Init(json))?;

        Ok(())
    }

    /// Handle the initialize response: extract capabilities, send `initialized`.
    ///
    /// Per LSP 3.17 §3.4.2: after receiving InitializeResult, client
    /// sends `initialized` notification, then `workspace/didChangeConfiguration`.
    async fn handle_initialize_result(&self, result: serde_json::Value) {
        // Extract server capabilities
        if let Some(caps) = extract_server_capabilities(&result) {
            let flags = ServerCapabilityFlags::from_capabilities(&caps);
            *self.capability_flags.write().await = flags;
            *self.server_capabilities.write().await = Some(caps);
        }

        // Send `initialized` notification (§3.4.2)
        let json = types::build_notification("initialized", serde_json::json!({}));
        let _ = self.writer_tx.send(OutgoingMessage::Init(json));

        // Send `workspace/didChangeConfiguration` (§3.4.3)
        let settings = self.server_info.get("settings")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        let json = types::build_notification(
            "workspace/didChangeConfiguration",
            serde_json::json!({"settings": settings}),
        );
        let _ = self.writer_tx.send(OutgoingMessage::Init(json));

        // Signal that init is complete — normal messages can flow
        self.initialized.notify_one();

        tracing::info!("LSP server '{}' initialized", self.server_name);
    }

    /// Send a request to the LSP server and await the response.
    ///
    /// Per JSON-RPC 2.0: requests have an `id` field and expect a response.
    pub async fn send_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let id = self.id_gen.next();
        let (tx, rx) = oneshot::channel();
        self.pending_requests.insert(id, tx);

        let json = types::build_request(id, method, params);
        self.writer_tx.send(OutgoingMessage::Normal(json))?;

        let result = rx.await.context("server disconnected")?;
        Ok(result)
    }

    /// Send a request without waiting for response (for handler dispatch).
    ///
    /// Returns the request ID so the caller can track responses.
    pub fn send_request_no_wait(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<u64> {
        let id = self.id_gen.next();
        let json = types::build_request(id, method, params);
        self.writer_tx.send(OutgoingMessage::Normal(json))?;
        Ok(id)
    }

    /// Register a pending request handler for a request ID.
    pub fn register_pending(&self, id: u64) -> oneshot::Receiver<serde_json::Value> {
        let (tx, rx) = oneshot::channel();
        self.pending_requests.insert(id, tx);
        rx
    }

    /// Send a notification to the LSP server.
    ///
    /// Per JSON-RPC 2.0: notifications have no `id` and expect no response.
    pub fn send_notification(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<()> {
        let json = types::build_notification(method, params);
        self.writer_tx.send(OutgoingMessage::Normal(json))?;
        Ok(())
    }

    /// Send a response to a server-initiated request.
    pub fn send_response(
        &self,
        request_id: u64,
        result: serde_json::Value,
    ) -> Result<()> {
        let json = types::build_response(request_id, result);
        self.writer_tx.send(OutgoingMessage::Normal(json))?;
        Ok(())
    }

    /// Send `textDocument/didOpen` notification (§3.4.5).
    pub fn send_did_open(
        &self,
        uri: &str,
        language_id: &str,
        version: i32,
        text: &str,
    ) -> Result<()> {
        self.send_notification("textDocument/didOpen", serde_json::json!({
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": version,
                "text": text
            }
        }))
    }

    /// Send `textDocument/didClose` notification (§3.4.6).
    pub fn send_did_close(&self, uri: &str) -> Result<()> {
        self.send_notification("textDocument/didClose", serde_json::json!({
            "textDocument": {"uri": uri}
        }))
    }

    /// Send incremental `textDocument/didChange` notification (§3.4.7).
    pub fn send_did_change_incremental(
        &self,
        uri: &str,
        version: i32,
        range_start: serde_json::Value,
        range_end: serde_json::Value,
        range_length: u64,
        text: &str,
    ) -> Result<()> {
        self.send_notification("textDocument/didChange", serde_json::json!({
            "textDocument": {"uri": uri, "version": version},
            "contentChanges": [{
                "range": {"start": range_start, "end": range_end},
                "rangeLength": range_length,
                "text": text
            }]
        }))
    }

    /// Send full content `textDocument/didChange` notification (§3.4.7).
    pub fn send_did_change_full(
        &self,
        uri: &str,
        version: i32,
        text: &str,
    ) -> Result<()> {
        self.send_notification("textDocument/didChange", serde_json::json!({
            "textDocument": {"uri": uri, "version": version},
            "contentChanges": [{"text": text}]
        }))
    }

    /// Send `textDocument/didSave` notification (§3.4.8).
    pub fn send_did_save(&self, uri: &str, text: Option<&str>) -> Result<()> {
        let mut params = serde_json::json!({"textDocument": {"uri": uri}});
        if let Some(text) = text {
            params["text"] = serde_json::Value::String(text.to_string());
        }
        self.send_notification("textDocument/didSave", params)
    }

    /// Send `shutdown` request (§3.4.3).
    pub async fn shutdown(&self) -> Result<()> {
        self.send_request("shutdown", serde_json::Value::Null).await?;
        // Send `exit` notification (§3.4.4)
        let json = types::build_notification("exit", serde_json::json!({}));
        let _ = self.writer_tx.send(OutgoingMessage::Normal(json));
        Ok(())
    }
}

/// Writer loop: sends Content-Length framed messages to LSP server stdin.
///
/// Per LSP 3.17 initialization protocol:
/// 1. Send the first init message (initialize request)
/// 2. Wait for initialized notification
/// 3. Send remaining init messages (initialized, didChangeConfiguration)
/// 4. Then process normal messages
async fn writer_loop(
    mut stdin: tokio::process::ChildStdin,
    mut rx: mpsc::UnboundedReceiver<OutgoingMessage>,
    initialized: Arc<Notify>,
) {
    // Phase 1: send the `initialize` request (first init message)
    if let Some(msg) = rx.recv().await {
        if let OutgoingMessage::Init(json) = msg {
            let frame = transport::encode(&json);
            if let Err(e) = stdin.write_all(&frame).await {
                tracing::error!("LSP write error: {}", e);
                return;
            }
            let _ = stdin.flush().await;
        }
    }

    // Phase 2: wait for initialization to complete
    initialized.notified().await;

    // Phase 3: drain remaining init messages
    while let Ok(msg) = rx.try_recv() {
        let json = match msg {
            OutgoingMessage::Init(j) | OutgoingMessage::Normal(j) => j,
        };
        let frame = transport::encode(&json);
        if let Err(e) = stdin.write_all(&frame).await {
            tracing::error!("LSP write error: {}", e);
            return;
        }
        let _ = stdin.flush().await;
    }

    // Phase 4: normal operation
    while let Some(msg) = rx.recv().await {
        let json = match msg {
            OutgoingMessage::Init(j) | OutgoingMessage::Normal(j) => j,
        };
        let frame = transport::encode(&json);
        if let Err(e) = stdin.write_all(&frame).await {
            tracing::error!("LSP write error: {}", e);
            return;
        }
        let _ = stdin.flush().await;
    }
}

/// Reader loop: reads Content-Length framed messages from LSP server stdout.
async fn reader_loop(
    stdout: tokio::process::ChildStdout,
    pending_requests: Arc<DashMap<u64, oneshot::Sender<serde_json::Value>>>,
    on_notification: Arc<NotificationCallback>,
    on_server_request: Arc<ServerRequestCallback>,
    server: std::sync::Weak<LspServer>,
) {
    let mut decoder = transport::ContentLengthDecoder::new();
    let mut buf = [0u8; 65536];
    let mut stdout = stdout;

    loop {
        match stdout.read(&mut buf).await {
            Ok(0) => {
                tracing::info!("LSP server stdout closed");
                break;
            }
            Ok(n) => {
                decoder.push(&buf[..n]);
                loop {
                    match decoder.next_message() {
                        Ok(Some(json_str)) => {
                            match IncomingMessage::parse(&json_str) {
                                Ok(msg) => {
                                    handle_incoming(
                                        msg,
                                        &pending_requests,
                                        &on_notification,
                                        &on_server_request,
                                        &server,
                                    )
                                    .await;
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to parse LSP message: {}", e);
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::error!("LSP transport decode error: {}", e);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!("LSP stdout read error: {}", e);
                break;
            }
        }
    }
}

/// Handle a single incoming LSP message.
async fn handle_incoming(
    msg: IncomingMessage,
    pending_requests: &Arc<DashMap<u64, oneshot::Sender<serde_json::Value>>>,
    on_notification: &Arc<NotificationCallback>,
    on_server_request: &Arc<ServerRequestCallback>,
    server: &std::sync::Weak<LspServer>,
) {
    match msg {
        IncomingMessage::Response { id, result } => {
            // Check if this is the initialize response
            if let Some(srv) = server.upgrade() {
                if id == srv.initialize_id {
                    srv.handle_initialize_result(result).await;
                    return;
                }
            }
            // Normal response: resolve pending request
            if let Some((_, tx)) = pending_requests.remove(&id) {
                let _ = tx.send(result);
            }
        }
        IncomingMessage::ErrorResponse { id, error } => {
            tracing::warn!("LSP error response (id={}): {} (code={})", id, error.message, error.code);
            if let Some((_, tx)) = pending_requests.remove(&id) {
                let _ = tx.send(serde_json::json!({"error": {
                    "code": error.code,
                    "message": error.message
                }}));
            }
        }
        IncomingMessage::Notification { method, params } => {
            on_notification(method, params);
        }
        IncomingMessage::Request { id, method, params } => {
            // Server-initiated requests — must respond per LSP 3.17.
            // Handle common ones automatically, delegate the rest.
            let response_json = match method.as_str() {
                // §3.6.4: workspace/configuration — return settings
                "workspace/configuration" => {
                    // Return empty config for each requested item
                    let items_len = params.get("items")
                        .and_then(|i| i.as_array())
                        .map(|a| a.len())
                        .unwrap_or(1);
                    Some(types::build_response(id, serde_json::json!(
                        vec![serde_json::Value::Object(serde_json::Map::new()); items_len]
                    )))
                }
                // §3.18.20: window/workDoneProgress/create — acknowledge
                "window/workDoneProgress/create" => {
                    Some(types::build_response(id, serde_json::Value::Null))
                }
                // §3.7.1: client/registerCapability — acknowledge
                "client/registerCapability" => {
                    Some(types::build_response(id, serde_json::Value::Null))
                }
                // §3.18.21: workspace/applyEdit — acknowledge
                "workspace/applyEdit" => {
                    Some(types::build_response(id, serde_json::json!({"applied": true})))
                }
                // workspace/diagnostic/refresh — acknowledge
                "workspace/diagnostic/refresh" => {
                    Some(types::build_response(id, serde_json::Value::Null))
                }
                _ => None,
            };

            if let Some(json) = response_json {
                // Send response directly via the writer
                if let Some(srv) = server.upgrade() {
                    let _ = srv.writer_tx.send(OutgoingMessage::Normal(json));
                }
            }

            // Also notify the callback for custom handling
            on_server_request(id, method, params);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outgoing_message_init_vs_normal() {
        let init = OutgoingMessage::Init("test".to_string());
        let normal = OutgoingMessage::Normal("test".to_string());
        match init {
            OutgoingMessage::Init(s) => assert_eq!(s, "test"),
            _ => panic!("expected Init"),
        }
        match normal {
            OutgoingMessage::Normal(s) => assert_eq!(s, "test"),
            _ => panic!("expected Normal"),
        }
    }

    // Integration tests for LspServer require a real LSP server binary.
    // These are covered in end-to-end tests, not unit tests.
    // The capabilities module tests cover the initialize response parsing.
}
