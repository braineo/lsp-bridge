//! lsp-bridge Rust backend entry point.
//!
//! Usage: lsp-bridge <emacs-epc-port>
//!
//! 1. Connects to Emacs EPC server on the given port
//! 2. Starts its own EPC server for Emacs to call
//! 3. Notifies Emacs of the server port via lsp-bridge--first-start
//! 4. Processes EPC calls until Emacs disconnects

mod bridge;
mod config;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::RwLock;
use tracing;
use tracing_subscriber;

use epc::client::EpcClient;
use epc::server::EpcServer;
use epc::sexp::SexpValue;
use epc::types::EvalArg;

use bridge::LspBridge;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("lsp_bridge=info".parse().unwrap()),
        )
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: lsp-bridge <emacs-epc-port>");
        eprintln!("       lsp-bridge is started by Emacs, not manually.");
        std::process::exit(1);
    }

    let emacs_port: u16 = args[1]
        .parse()
        .context("invalid EPC port number")?;

    // Determine base directory (where langserver/ and multiserver/ dirs are)
    let exe_path = std::env::current_exe().unwrap_or_default();
    let base_dir = exe_path
        .parent()  // rust/target/release/
        .and_then(|p| p.parent())  // rust/target/
        .and_then(|p| p.parent())  // rust/
        .and_then(|p| p.parent())  // lsp-bridge/
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    tracing::info!(
        "lsp-bridge starting: emacs_port={}, base_dir={}",
        emacs_port,
        base_dir.display()
    );

    // STEP 1: Connect to Emacs's EPC server as a client.
    // Python: init_epc_client(int(args[0]))
    // Emacs has already started an EPC server; we connect to it.
    let emacs_conn = epc::server::EpcConnection::connect("127.0.0.1", emacs_port).await?;
    tracing::info!("Connected to Emacs EPC server on port {}", emacs_port);

    // Wrap in EpcClient for high-level API (eval_in_emacs, get_emacs_vars, etc.)
    let emacs_client = Arc::new(EpcClient::new(emacs_conn));

    // STEP 2: Create our own EPC server for Emacs to call us.
    // Python: self.server = ThreadingEPCServer(('127.0.0.1', 0))
    let epc_server = EpcServer::new().await?;
    let server_port = epc_server.port();
    tracing::info!("EPC server listening on port {}", server_port);

    // STEP 3: Create bridge and inject Emacs client.
    let mut bridge_inner = LspBridge::new(base_dir);
    bridge_inner.set_emacs(emacs_client.clone());
    let bridge = Arc::new(RwLock::new(bridge_inner));

    // STEP 4: Create serial event queue (matches Python's event_dispatcher).
    // All file action methods go through this channel to prevent concurrent
    // operations on the same workspace (e.g., spawning duplicate LSP servers).
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<EventMessage>(256);

    // Event dispatcher task — processes events sequentially.
    // This is the Rust equivalent of Python's event_dispatcher thread.
    // All file operations are serialized here to prevent race conditions.
    let bridge_for_events = bridge.clone();
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            let bridge = bridge_for_events.read().await;
            match event.method.as_str() {
                "open_file" => {
                    let filepath = event.args.first()
                        .and_then(|a| a.as_str())
                        .unwrap_or("")
                        .to_string();
                    if let Err(e) = bridge.open_file(&filepath).await {
                        tracing::error!("open_file error: {}", e);
                    }
                }
                "close_file" => {
                    let filepath = event.args.first()
                        .and_then(|a| a.as_str())
                        .unwrap_or("")
                        .to_string();
                    if let Err(e) = bridge.close_file(&filepath).await {
                        tracing::error!("close_file error: {}", e);
                    }
                }
                method => {
                    if let Err(e) = bridge.dispatch(method, event.args).await {
                        tracing::error!("event dispatch '{}' error: {}", method, e);
                    }
                }
            }
        }
        tracing::info!("Event dispatcher exited");
    });

    // STEP 5: Register EPC methods on our server.
    register_methods(&epc_server, bridge.clone(), event_tx.clone());

    // STEP 5: Notify Emacs of our server port.
    // Python: eval_in_emacs('lsp-bridge--first-start', self.server.server_address[1])
    // This tells Emacs to connect to our EPC server.
    emacs_client
        .eval_in_emacs(
            "lsp-bridge--first-start",
            &[EvalArg::Integer(server_port as i64)],
        )
        .await?;
    tracing::info!("Notified Emacs: lsp-bridge--first-start {}", server_port);

    // STEP 6: Accept the connection from Emacs to our EPC server.
    // After receiving lsp-bridge--first-start, Emacs calls lsp-bridge-epc-connect
    // to connect to our server port.
    let _epc_conn = epc_server.accept().await?;
    tracing::info!("Emacs connected to our EPC server");

    // Keep running until Emacs disconnects
    loop {
        if !emacs_client.is_alive() {
            tracing::info!("Emacs disconnected, shutting down");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    // Graceful shutdown: send shutdown+exit to all LSP servers
    tracing::info!("Shutting down all LSP servers...");
    {
        let bridge = bridge.read().await;
        for entry in bridge.lsp_servers.iter() {
            let server = entry.value();
            tracing::info!("Shutting down LSP server: {}", server.server_name);
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                server.shutdown(),
            ).await {
                Ok(Ok(())) => tracing::info!("  {} shutdown OK", server.server_name),
                Ok(Err(e)) => tracing::warn!("  {} shutdown error: {}", server.server_name, e),
                Err(_) => tracing::warn!("  {} shutdown timed out", server.server_name),
            }
        }
    }

    tracing::info!("lsp-bridge exiting");
    Ok(())
}

/// An event message for the serial event queue.
struct EventMessage {
    method: String,
    args: Vec<SexpValue>,
}

/// Register all EPC methods on the server.
///
/// File action methods go through the event queue (serialized, like Python's
/// event_dispatcher). Other methods run directly.
fn register_methods(
    server: &EpcServer,
    bridge: Arc<RwLock<LspBridge>>,
    event_tx: tokio::sync::mpsc::Sender<EventMessage>,
) {
    // File action methods — all go through the serial event queue.
    // This matches Python's event_dispatcher pattern: prevents concurrent
    // operations from spawning duplicate LSP servers for the same workspace.

    // Methods that go through the event queue (serialized):
    let file_action_methods: Vec<&str> = {
        let bridge = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(bridge.read())
        });
        let mut methods: Vec<&str> = bridge.handlers.keys().copied().collect();
        methods.extend(&[
            "change_file", "update_file", "save_file",
            "try_completion", "try_formatting", "change_cursor",
            "list_diagnostics", "try_code_action", "workspace_symbol",
        ]);
        methods
    };

    for method_name in file_action_methods {
        let tx = event_tx.clone();
        // Map Emacs method names to handler names
        let handler_name = match method_name {
            "try_completion" => "completion",
            "try_code_action" => "code_action",
            other => other,
        };
        let handler_name = handler_name.to_string();

        server.register(method_name, move |args| {
            let tx = tx.clone();
            let handler_name = handler_name.clone();
            async move {
                // Push to event queue — processed sequentially
                let _ = tx.send(EventMessage {
                    method: handler_name,
                    args,
                }).await;
                Ok(SexpValue::Nil)
            }
        });
    }

    // open_file and close_file also go through the event queue
    {
        let tx = event_tx.clone();
        server.register("open_file", move |args| {
            let tx = tx.clone();
            async move {
                let _ = tx.send(EventMessage {
                    method: "open_file".to_string(),
                    args,
                }).await;
                Ok(SexpValue::Nil)
            }
        });
    }
    {
        let tx = event_tx.clone();
        server.register("close_file", move |args| {
            let tx = tx.clone();
            async move {
                let _ = tx.send(EventMessage {
                    method: "close_file".to_string(),
                    args,
                }).await;
                Ok(SexpValue::Nil)
            }
        });
    }

    // Search backend stubs — Emacs calls these right after connection.
    // Python: build_prefix_function registers "search_file_words_index_files", etc.
    let search_methods = [
        "search_file_words_index_files",
        "search_file_words_change_buffer",
        "search_file_words_load_file",
        "search_file_words_close_file",
        "search_file_words_search",
        "search_sdcv_words_search",
        "search_list_search",
        "search_list_update",
        "search_paths_search",
    ];
    for method in &search_methods {
        server.register(*method, move |args| {
            async move {
                tracing::debug!("Search method called: {} ({} args)", stringify!($method), args.len());
                Ok(SexpValue::Nil)
            }
        });
    }

    // Other methods Emacs may call
    let stub_methods = [
        "close_all_files",
        "rename_file",
        "fetch_completion_item_info",
        "tabnine_complete",
        "copilot_complete",
        "copilot_login",
        "copilot_logout",
        "copilot_status",
        "copilot_completion_accept",
        "codeium_complete",
        "codeium_completion_accept",
        "codeium_auth",
        "codeium_get_api_key",
        "ctags_complete",
        "ctags_find_def",
        "cleanup",
        "profile_dump",
    ];
    for method in &stub_methods {
        server.register(*method, move |_args| {
            async move {
                Ok(SexpValue::Nil)
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_creates_with_handlers() {
        let bridge = LspBridge::new(PathBuf::from("/tmp"));
        assert!(!bridge.handlers.is_empty());
    }

    #[test]
    fn config_module_accessible() {
        // Verify config module is accessible
        let _config = config::ServerConfig {
            name: "test".to_string(),
            language_id: "test".to_string(),
            language_ids: std::collections::HashMap::new(),
            command: vec!["test-server".to_string()],
            settings: serde_json::Value::Null,
            project_files: vec![],
            support_single_file: false,
            capabilities: serde_json::Value::Null,
            initialization_options: serde_json::Value::Null,
            org_babel_virtual_file: None,
            completion_trigger_characters: None,
        };
    }
}
