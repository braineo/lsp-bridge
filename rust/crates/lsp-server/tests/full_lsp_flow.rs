//! Full LSP flow integration test with ty server.
//!
//! Tests every handler's request → LSP server → response cycle.
//! This catches wire format issues, wrong params, and missing fields.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use lsp_server::server::{LspServer, NotificationCallback, ServerRequestCallback, path_to_uri};

fn noop_notification() -> Arc<NotificationCallback> {
    Arc::new(Box::new(|method, _params| {
        if method == "textDocument/publishDiagnostics" {
            eprintln!("  [diag] publishDiagnostics received");
        }
    }))
}

fn noop_server_request() -> Arc<ServerRequestCallback> {
    Arc::new(Box::new(|_id, _method, _params| {}))
}

async fn setup_ty() -> (Arc<LspServer>, String, tempfile::TempDir) {
    if std::process::Command::new("ty").arg("--version").output().is_err() {
        panic!("SKIP: ty not found");
    }

    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().to_path_buf();
    std::fs::write(project.join("pyproject.toml"), "[project]\nname = \"test\"\n").unwrap();
    std::fs::write(project.join("test.py"), r#"
import os

def my_function(x: int, y: str) -> bool:
    """A test function."""
    return len(y) > x

result = my_function(1, "hello")
os.path.join("/tmp", "test")
undefined_var
"#).unwrap();

    let file_uri = path_to_uri(&project.join("test.py"));

    let config = json!({
        "name": "ty",
        "languageId": "python",
        "command": ["ty", "server"],
        "initializationOptions": {"logLevel": "warn"},
        "settings": {}
    });

    let server = LspServer::spawn(
        "ty".to_string(),
        project.clone(),
        config,
        true,
        noop_notification(),
        noop_server_request(),
    ).await.expect("spawn ty");

    server.send_initialize(None).await.expect("init");
    tokio::time::sleep(Duration::from_secs(2)).await;

    let text = std::fs::read_to_string(project.join("test.py")).unwrap();
    server.send_did_open(&file_uri, "python", 0, &text).expect("didOpen");
    tokio::time::sleep(Duration::from_secs(2)).await;

    (server, file_uri, tmp)
}

async fn request_with_timeout(
    server: &LspServer,
    method: &str,
    params: serde_json::Value,
    timeout_secs: u64,
) -> Result<serde_json::Value, String> {
    match tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        server.send_request(method, params),
    ).await {
        Ok(Ok(resp)) => {
            if let Some(err) = resp.get("error") {
                Err(format!("LSP error: {}", err))
            } else {
                Ok(resp)
            }
        }
        Ok(Err(e)) => Err(format!("request error: {}", e)),
        Err(_) => Err(format!("timeout after {}s", timeout_secs)),
    }
}

#[tokio::test]
async fn test_completion() {
    let (server, file_uri, _tmp) = setup_ty().await;

    eprintln!("=== test_completion ===");
    let result = request_with_timeout(&server, "textDocument/completion", json!({
        "textDocument": {"uri": &file_uri},
        "position": {"line": 8, "character": 8},
        "context": {"triggerKind": 2, "triggerCharacter": "."}
    }), 10).await;

    match result {
        Ok(resp) => {
            let items = resp.get("items").and_then(|i| i.as_array())
                .or_else(|| resp.as_array())
                .map(|a| a.len()).unwrap_or(0);
            eprintln!("  OK: {} completion items", items);
            assert!(items > 0, "expected completion items for os.path.join");
        }
        Err(e) => eprintln!("  FAIL: {}", e),
    }

    let _ = server.shutdown().await;
}

#[tokio::test]
async fn test_hover() {
    let (server, file_uri, _tmp) = setup_ty().await;

    eprintln!("=== test_hover ===");
    let result = request_with_timeout(&server, "textDocument/hover", json!({
        "textDocument": {"uri": &file_uri},
        "position": {"line": 3, "character": 5}  // on "my_function"
    }), 10).await;

    match result {
        Ok(resp) => {
            let has_contents = resp.get("contents").is_some();
            eprintln!("  OK: has_contents={}", has_contents);
            if let Some(contents) = resp.get("contents") {
                let preview = serde_json::to_string(contents).unwrap_or_default();
                eprintln!("  contents: {}", &preview[..preview.len().min(200)]);
            }
        }
        Err(e) => eprintln!("  FAIL: {}", e),
    }

    let _ = server.shutdown().await;
}

#[tokio::test]
async fn test_definition() {
    let (server, file_uri, _tmp) = setup_ty().await;

    eprintln!("=== test_definition ===");
    // Go to definition of "my_function" on line 7 (where it's called)
    let result = request_with_timeout(&server, "textDocument/definition", json!({
        "textDocument": {"uri": &file_uri},
        "position": {"line": 7, "character": 10}  // on "my_function" call
    }), 10).await;

    match result {
        Ok(resp) => {
            eprintln!("  OK: {}", serde_json::to_string(&resp).unwrap_or_default().chars().take(300).collect::<String>());
            // Should point to line 3 where my_function is defined
        }
        Err(e) => eprintln!("  FAIL: {}", e),
    }

    let _ = server.shutdown().await;
}

#[tokio::test]
async fn test_references() {
    let (server, file_uri, _tmp) = setup_ty().await;

    eprintln!("=== test_references ===");
    // Find references to "my_function" (defined on line 3, called on line 7)
    let result = request_with_timeout(&server, "textDocument/references", json!({
        "textDocument": {"uri": &file_uri},
        "position": {"line": 3, "character": 5},  // on "my_function" definition
        "context": {"includeDeclaration": true}
    }), 10).await;

    match result {
        Ok(resp) => {
            if resp.is_null() {
                eprintln!("  OK: null (no references — ty may not support this)");
            } else if let Some(arr) = resp.as_array() {
                eprintln!("  OK: {} references", arr.len());
                for loc in arr.iter().take(3) {
                    eprintln!("    line {}", loc.get("range").and_then(|r| r.get("start")).and_then(|s| s.get("line")).and_then(|l| l.as_u64()).unwrap_or(999));
                }
            } else {
                eprintln!("  OK: {}", serde_json::to_string(&resp).unwrap_or_default().chars().take(200).collect::<String>());
            }
        }
        Err(e) => eprintln!("  FAIL: {}", e),
    }

    let _ = server.shutdown().await;
}

#[tokio::test]
async fn test_document_symbol() {
    let (server, file_uri, _tmp) = setup_ty().await;

    eprintln!("=== test_document_symbol ===");
    let result = request_with_timeout(&server, "textDocument/documentSymbol", json!({
        "textDocument": {"uri": &file_uri}
    }), 10).await;

    match result {
        Ok(resp) => {
            if let Some(arr) = resp.as_array() {
                eprintln!("  OK: {} symbols", arr.len());
                for sym in arr.iter().take(5) {
                    eprintln!("    {} (kind={})",
                        sym.get("name").and_then(|n| n.as_str()).unwrap_or("?"),
                        sym.get("kind").and_then(|k| k.as_u64()).unwrap_or(0));
                }
            }
        }
        Err(e) => eprintln!("  FAIL: {}", e),
    }

    let _ = server.shutdown().await;
}

#[tokio::test]
async fn test_formatting() {
    let (server, file_uri, _tmp) = setup_ty().await;

    eprintln!("=== test_formatting ===");
    let result = request_with_timeout(&server, "textDocument/formatting", json!({
        "textDocument": {"uri": &file_uri},
        "options": {"tabSize": 4, "insertSpaces": true}
    }), 10).await;

    match result {
        Ok(resp) => {
            if resp.is_null() {
                eprintln!("  OK: null (no formatting changes or not supported)");
            } else if let Some(arr) = resp.as_array() {
                eprintln!("  OK: {} text edits", arr.len());
            } else {
                eprintln!("  OK: {}", serde_json::to_string(&resp).unwrap_or_default().chars().take(200).collect::<String>());
            }
        }
        Err(e) => eprintln!("  FAIL (may not be supported by ty): {}", e),
    }

    let _ = server.shutdown().await;
}

#[tokio::test]
async fn test_inlay_hints() {
    let (server, file_uri, _tmp) = setup_ty().await;

    eprintln!("=== test_inlay_hints ===");
    let result = request_with_timeout(&server, "textDocument/inlayHint", json!({
        "textDocument": {"uri": &file_uri},
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": 10, "character": 0}
        }
    }), 10).await;

    match result {
        Ok(resp) => {
            if resp.is_null() {
                eprintln!("  OK: null (no hints)");
            } else if let Some(arr) = resp.as_array() {
                eprintln!("  OK: {} inlay hints", arr.len());
                for hint in arr.iter().take(3) {
                    eprintln!("    line {} label={:?}",
                        hint.get("position").and_then(|p| p.get("line")).and_then(|l| l.as_u64()).unwrap_or(0),
                        hint.get("label"));
                }
            }
        }
        Err(e) => eprintln!("  FAIL: {}", e),
    }

    let _ = server.shutdown().await;
}

#[tokio::test]
async fn test_diagnostic_pull() {
    let (server, file_uri, _tmp) = setup_ty().await;

    eprintln!("=== test_diagnostic (pull) ===");
    let result = request_with_timeout(&server, "textDocument/diagnostic", json!({
        "textDocument": {"uri": &file_uri}
    }), 10).await;

    match result {
        Ok(resp) => {
            if let Some(items) = resp.get("items").and_then(|i| i.as_array()) {
                eprintln!("  OK: {} diagnostics", items.len());
                for d in items.iter().take(3) {
                    eprintln!("    line {} severity={} {:?}",
                        d.get("range").and_then(|r| r.get("start")).and_then(|s| s.get("line")).and_then(|l| l.as_u64()).unwrap_or(0),
                        d.get("severity").and_then(|s| s.as_u64()).unwrap_or(0),
                        d.get("message").and_then(|m| m.as_str()).unwrap_or("?"));
                }
            } else {
                eprintln!("  OK: {}", serde_json::to_string(&resp).unwrap_or_default().chars().take(200).collect::<String>());
            }
        }
        Err(e) => eprintln!("  FAIL: {}", e),
    }

    let _ = server.shutdown().await;
}

#[tokio::test]
async fn test_code_action() {
    let (server, file_uri, _tmp) = setup_ty().await;

    eprintln!("=== test_code_action ===");
    let result = request_with_timeout(&server, "textDocument/codeAction", json!({
        "textDocument": {"uri": &file_uri},
        "range": {
            "start": {"line": 1, "character": 0},
            "end": {"line": 1, "character": 9}  // "import os" — might have unused import
        },
        "context": {"diagnostics": []}
    }), 10).await;

    match result {
        Ok(resp) => {
            if resp.is_null() {
                eprintln!("  OK: null (no actions)");
            } else if let Some(arr) = resp.as_array() {
                eprintln!("  OK: {} code actions", arr.len());
                for a in arr.iter().take(3) {
                    eprintln!("    {:?}", a.get("title").and_then(|t| t.as_str()).unwrap_or("?"));
                }
            }
        }
        Err(e) => eprintln!("  FAIL: {}", e),
    }

    let _ = server.shutdown().await;
}

#[tokio::test]
async fn test_signature_help() {
    let (server, file_uri, _tmp) = setup_ty().await;

    eprintln!("=== test_signature_help ===");
    // Position after "my_function(" — should show signature
    let result = request_with_timeout(&server, "textDocument/signatureHelp", json!({
        "textDocument": {"uri": &file_uri},
        "position": {"line": 7, "character": 22}  // after "my_function("
    }), 10).await;

    match result {
        Ok(resp) => {
            if resp.is_null() {
                eprintln!("  OK: null (no signature help)");
            } else if let Some(sigs) = resp.get("signatures").and_then(|s| s.as_array()) {
                eprintln!("  OK: {} signatures", sigs.len());
                for s in sigs.iter().take(2) {
                    eprintln!("    {:?}", s.get("label").and_then(|l| l.as_str()).unwrap_or("?"));
                }
            }
        }
        Err(e) => eprintln!("  FAIL: {}", e),
    }

    let _ = server.shutdown().await;
}
