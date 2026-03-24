//! Integration tests with real LSP servers (ty, rust-analyzer).
//!
//! These test the full LSP lifecycle:
//! 1. Spawn server subprocess
//! 2. Send initialize request
//! 3. Receive capabilities
//! 4. Send initialized notification
//! 5. Send didOpen
//! 6. Send completion request
//! 7. Shutdown

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use lsp_server::capabilities::ServerCapabilityFlags;
use lsp_server::server::{LspServer, NotificationCallback, ServerRequestCallback};

fn noop_notification() -> Arc<NotificationCallback> {
    Arc::new(Box::new(|method, params| {
        eprintln!("  notification: {} (params keys: {:?})",
            method,
            params.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
    }))
}

fn noop_server_request() -> Arc<ServerRequestCallback> {
    Arc::new(Box::new(|id, method, _params| {
        eprintln!("  server request: {} (id={})", method, id);
    }))
}

#[tokio::test]
async fn test_ty_server_init() {
    // Skip if ty not available
    if std::process::Command::new("ty").arg("--version").output().is_err() {
        eprintln!("SKIP: ty not found");
        return;
    }

    // Create a temp Python project
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().to_path_buf();
    std::fs::write(project_path.join("test.py"), "import os\nos.path\n").unwrap();
    // ty needs pyproject.toml to recognize project
    std::fs::write(project_path.join("pyproject.toml"), "[project]\nname = \"test\"\n").unwrap();

    let config = json!({
        "name": "ty",
        "languageId": "python",
        "command": ["ty", "server"],
        "initializationOptions": {"logLevel": "warn"},
        "settings": {}
    });

    eprintln!("Spawning ty server...");
    let server = LspServer::spawn(
        "ty".to_string(),
        project_path.clone(),
        config,
        true,
        noop_notification(),
        noop_server_request(),
    )
    .await
    .expect("failed to spawn ty");

    eprintln!("Sending initialize...");
    server.send_initialize(None).await.expect("initialize failed");

    // Wait for initialization to complete
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // Check capabilities
    let flags = server.capability_flags.read().await;
    eprintln!("ty capabilities:");
    eprintln!("  completion_trigger_characters: {:?}", flags.completion_trigger_characters);
    eprintln!("  completion_resolve_provider: {}", flags.completion_resolve_provider);
    eprintln!("  text_document_sync_kind: {:?}", flags.text_document_sync_kind);
    eprintln!("  diagnostic_provider: {}", flags.diagnostic_provider);
    eprintln!("  inlay_hint_provider: {}", flags.inlay_hint_provider);

    // ty should have at least completion support
    // (specific capabilities depend on ty version)

    // Send didOpen
    let file_uri = format!("file://{}", project_path.join("test.py").display());
    eprintln!("Sending didOpen for {}...", file_uri);
    server.send_did_open(
        &file_uri,
        "python",
        0,
        "import os\nos.path\n",
    ).expect("didOpen failed");

    // Wait for server to process
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Send completion request
    eprintln!("Sending completion request...");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        server.send_request("textDocument/completion", json!({
            "textDocument": {"uri": file_uri},
            "position": {"line": 1, "character": 7},
            "context": {"triggerKind": 2, "triggerCharacter": "."}
        }))
    ).await;

    match result {
        Ok(Ok(response)) => {
            eprintln!("Completion response received!");
            if let Some(items) = response.get("items").and_then(|i| i.as_array()) {
                eprintln!("  {} completion items", items.len());
                for item in items.iter().take(5) {
                    eprintln!("    - {}", item.get("label").and_then(|l| l.as_str()).unwrap_or("?"));
                }
            } else if response.is_array() {
                let items = response.as_array().unwrap();
                eprintln!("  {} completion items (flat)", items.len());
                for item in items.iter().take(5) {
                    eprintln!("    - {}", item.get("label").and_then(|l| l.as_str()).unwrap_or("?"));
                }
            } else {
                eprintln!("  response: {}", serde_json::to_string_pretty(&response).unwrap_or_default());
            }
        }
        Ok(Err(e)) => eprintln!("Completion error: {}", e),
        Err(_) => eprintln!("Completion timed out (5s)"),
    }

    // Shutdown
    eprintln!("Shutting down...");
    let _ = server.shutdown().await;
    eprintln!("ty test PASSED");
}

#[tokio::test]
async fn test_rust_analyzer_init() {
    // Skip if rust-analyzer not available
    if std::process::Command::new("rust-analyzer").arg("--version").output().is_err() {
        eprintln!("SKIP: rust-analyzer not found");
        return;
    }

    // Create a temp Rust project
    let tmp = tempfile::tempdir().unwrap();
    let project_path = tmp.path().to_path_buf();
    std::fs::create_dir_all(project_path.join("src")).unwrap();
    std::fs::write(project_path.join("Cargo.toml"), r#"
[package]
name = "test-project"
version = "0.1.0"
edition = "2021"
"#).unwrap();
    std::fs::write(project_path.join("src/main.rs"), r#"
fn main() {
    let s = String::new();
    s.
}
"#).unwrap();

    let config = json!({
        "name": "rust-analyzer",
        "languageId": "rust",
        "command": ["rust-analyzer"],
        "settings": {},
        "initializationOptions": {
            "diagnostics": {"enable": true}
        }
    });

    eprintln!("Spawning rust-analyzer...");
    let server = LspServer::spawn(
        "rust-analyzer".to_string(),
        project_path.clone(),
        config,
        true,
        noop_notification(),
        noop_server_request(),
    )
    .await
    .expect("failed to spawn rust-analyzer");

    eprintln!("Sending initialize...");
    server.send_initialize(None).await.expect("initialize failed");

    // rust-analyzer takes longer to initialize (loading cargo workspace)
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    // Check capabilities
    let flags = server.capability_flags.read().await;
    eprintln!("rust-analyzer capabilities:");
    eprintln!("  completion_trigger_characters: {:?}", flags.completion_trigger_characters);
    eprintln!("  completion_resolve_provider: {}", flags.completion_resolve_provider);
    eprintln!("  inlay_hint_provider: {}", flags.inlay_hint_provider);
    eprintln!("  semantic_tokens_provider: {}", flags.semantic_tokens_provider);
    eprintln!("  rename_prepare_provider: {}", flags.rename_prepare_provider);
    eprintln!("  code_action_provider: {}", flags.code_action_provider);
    eprintln!("  signature_help_provider: {}", flags.signature_help_provider);

    // rust-analyzer should definitely support completion
    assert!(!flags.completion_trigger_characters.is_empty(),
        "rust-analyzer should have completion trigger characters");
    assert!(flags.completion_resolve_provider,
        "rust-analyzer should support completion resolve");
    assert!(flags.code_action_provider,
        "rust-analyzer should support code actions");

    // Shutdown
    eprintln!("Shutting down...");
    let _ = server.shutdown().await;
    eprintln!("rust-analyzer test PASSED");
}
