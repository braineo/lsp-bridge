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

    // STEP 3: Register EPC methods on our server.
    let bridge = Arc::new(RwLock::new(LspBridge::new(base_dir)));
    register_methods(&epc_server, bridge.clone());

    // STEP 4: Notify Emacs of our server port.
    // Python: eval_in_emacs('lsp-bridge--first-start', self.server.server_address[1])
    // This tells Emacs to connect to our EPC server.
    emacs_client
        .eval_in_emacs(
            "lsp-bridge--first-start",
            &[EvalArg::Integer(server_port as i64)],
        )
        .await?;
    tracing::info!("Notified Emacs: lsp-bridge--first-start {}", server_port);

    // STEP 5: Accept the connection from Emacs to our EPC server.
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

    Ok(())
}

/// Register all EPC methods on the server.
///
/// Mirrors Python's register_instance(self) + build_file_action_function.
fn register_methods(server: &EpcServer, bridge: Arc<RwLock<LspBridge>>) {
    // open_file(filepath)
    let b = bridge.clone();
    server.register("open_file", move |args| {
        let b = b.clone();
        async move {
            let filepath = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_string();
            let bridge = b.read().await;
            match bridge.open_file(&filepath).await {
                Ok(true) => Ok(SexpValue::Bool(true)),
                Ok(false) => Ok(SexpValue::Nil),
                Err(e) => {
                    tracing::error!("open_file error: {}", e);
                    Ok(SexpValue::Nil)
                }
            }
        }
    });

    // close_file(filepath)
    let b = bridge.clone();
    server.register("close_file", move |args| {
        let b = b.clone();
        async move {
            let filepath = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_string();
            let bridge = b.read().await;
            if let Err(e) = bridge.close_file(&filepath).await {
                tracing::error!("close_file error: {}", e);
            }
            Ok(SexpValue::Nil)
        }
    });

    // Register all handler methods (completion, hover, find_define, etc.)
    // These follow the build_file_action_function pattern from Python:
    // method(filepath, ...args) → dispatch to handler
    let handler_names: Vec<&'static str> = {
        let bridge = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(bridge.read())
        });
        bridge.handlers.keys().copied().collect()
    };

    for handler_name in handler_names {
        let b = bridge.clone();
        let name = handler_name.to_string();
        server.register(handler_name, move |args| {
            let b = b.clone();
            let name = name.clone();
            async move {
                let bridge = b.read().await;
                match bridge.dispatch(&name, args).await {
                    Ok(result) => Ok(result),
                    Err(e) => {
                        tracing::error!("handler '{}' error: {}", name, e);
                        Ok(SexpValue::Nil)
                    }
                }
            }
        });
    }

    // Additional methods matching Python's explicit registrations

    // change_file — file content changed
    let b = bridge.clone();
    server.register("change_file", move |args| {
        let b = b.clone();
        async move {
            let bridge = b.read().await;
            match bridge.dispatch("change_file", args).await {
                Ok(r) => Ok(r),
                Err(e) => {
                    tracing::error!("change_file error: {}", e);
                    Ok(SexpValue::Nil)
                }
            }
        }
    });

    // save_file
    let b = bridge.clone();
    server.register("save_file", move |args| {
        let b = b.clone();
        async move {
            let bridge = b.read().await;
            match bridge.dispatch("save_file", args).await {
                Ok(r) => Ok(r),
                Err(e) => {
                    tracing::error!("save_file error: {}", e);
                    Ok(SexpValue::Nil)
                }
            }
        }
    });

    // try_completion
    let b = bridge.clone();
    server.register("try_completion", move |args| {
        let b = b.clone();
        async move {
            let bridge = b.read().await;
            match bridge.dispatch("completion", args).await {
                Ok(r) => Ok(r),
                Err(e) => {
                    tracing::error!("try_completion error: {}", e);
                    Ok(SexpValue::Nil)
                }
            }
        }
    });

    // try_code_action
    let b = bridge.clone();
    server.register("try_code_action", move |args| {
        let b = b.clone();
        async move {
            let bridge = b.read().await;
            match bridge.dispatch("code_action", args).await {
                Ok(r) => Ok(r),
                Err(e) => {
                    tracing::error!("try_code_action error: {}", e);
                    Ok(SexpValue::Nil)
                }
            }
        }
    });
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
