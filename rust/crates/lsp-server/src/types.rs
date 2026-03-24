//! JSON-RPC 2.0 message types for LSP communication.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// A JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// A JSON-RPC 2.0 notification (no id, no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// A JSON-RPC 2.0 success response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    pub result: serde_json::Value,
}

/// A JSON-RPC 2.0 error response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcErrorResponse {
    pub jsonrpc: String,
    pub id: u64,
    pub error: JsonRpcError,
}

/// JSON-RPC error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// A parsed incoming JSON-RPC message (could be any type).
#[derive(Debug, Clone)]
pub enum IncomingMessage {
    /// Response to a request we sent.
    Response {
        id: u64,
        result: serde_json::Value,
    },
    /// Error response to a request we sent.
    ErrorResponse {
        id: u64,
        error: JsonRpcError,
    },
    /// Notification from the server (e.g., diagnostics).
    Notification {
        method: String,
        params: serde_json::Value,
    },
    /// Request from the server (e.g., workspace/configuration).
    Request {
        id: u64,
        method: String,
        params: serde_json::Value,
    },
}

impl IncomingMessage {
    /// Parse a JSON string into an IncomingMessage.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        let value: serde_json::Value = serde_json::from_str(json)?;

        let has_id = value.get("id").is_some();
        let has_method = value.get("method").is_some();
        let has_result = value.get("result").is_some();
        let has_error = value.get("error").is_some();

        if has_result && has_id {
            // Success response
            let id = value["id"].as_u64().unwrap_or(0);
            Ok(IncomingMessage::Response {
                id,
                result: value["result"].clone(),
            })
        } else if has_error && has_id {
            // Error response
            let id = value["id"].as_u64().unwrap_or(0);
            let error: JsonRpcError = serde_json::from_value(value["error"].clone())?;
            Ok(IncomingMessage::ErrorResponse { id, error })
        } else if has_method && has_id {
            // Server-initiated request
            let id = value["id"].as_u64().unwrap_or(0);
            let method = value["method"].as_str().unwrap_or("").to_string();
            let params = value.get("params").cloned().unwrap_or(serde_json::Value::Null);
            Ok(IncomingMessage::Request { id, method, params })
        } else if has_method && !has_id {
            // Notification
            let method = value["method"].as_str().unwrap_or("").to_string();
            let params = value.get("params").cloned().unwrap_or(serde_json::Value::Null);
            Ok(IncomingMessage::Notification { method, params })
        } else {
            // Unknown format — treat as notification
            Ok(IncomingMessage::Notification {
                method: String::new(),
                params: value,
            })
        }
    }
}

/// Build a JSON-RPC 2.0 request.
pub fn build_request(id: u64, method: &str, params: serde_json::Value) -> String {
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id,
        method: method.to_string(),
        params: Some(params),
    };
    serde_json::to_string(&req).unwrap()
}

/// Build a JSON-RPC 2.0 notification.
pub fn build_notification(method: &str, params: serde_json::Value) -> String {
    let notif = JsonRpcNotification {
        jsonrpc: "2.0".to_string(),
        method: method.to_string(),
        params: Some(params),
    };
    serde_json::to_string(&notif).unwrap()
}

/// Build a JSON-RPC 2.0 success response.
pub fn build_response(id: u64, result: serde_json::Value) -> String {
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result,
    };
    serde_json::to_string(&resp).unwrap()
}

/// Build a JSON-RPC 2.0 error response.
pub fn build_error_response(id: u64, code: i64, message: &str) -> String {
    let resp = JsonRpcErrorResponse {
        jsonrpc: "2.0".to_string(),
        id,
        error: JsonRpcError {
            code,
            message: message.to_string(),
            data: None,
        },
    };
    serde_json::to_string(&resp).unwrap()
}

/// Thread-safe request ID generator.
pub struct RequestIdGenerator {
    counter: AtomicU64,
}

impl RequestIdGenerator {
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(1),
        }
    }

    pub fn next(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::SeqCst)
    }
}

impl Default for RequestIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

// Standard LSP error codes
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
pub const SERVER_NOT_INITIALIZED: i64 = -32002;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ===== Build tests =====

    #[test]
    fn build_request_basic() {
        let json = build_request(1, "initialize", json!({"processId": 1234}));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["method"], "initialize");
        assert_eq!(parsed["params"]["processId"], 1234);
    }

    #[test]
    fn build_notification_basic() {
        let json = build_notification("initialized", json!({}));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["method"], "initialized");
        assert!(parsed.get("id").is_none());
    }

    #[test]
    fn build_response_basic() {
        let json = build_response(1, json!({"capabilities": {}}));
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 1);
        assert!(parsed["result"].is_object());
    }

    #[test]
    fn build_error_response_basic() {
        let json = build_error_response(1, METHOD_NOT_FOUND, "method not found");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["error"]["code"], -32601);
        assert_eq!(parsed["error"]["message"], "method not found");
    }

    // ===== Parse tests =====

    #[test]
    fn parse_response_success() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"items":[]}}"#;
        match IncomingMessage::parse(json).unwrap() {
            IncomingMessage::Response { id, result } => {
                assert_eq!(id, 1);
                assert!(result["items"].is_array());
            }
            other => panic!("expected Response, got: {:?}", other),
        }
    }

    #[test]
    fn parse_response_error() {
        let json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"not found"}}"#;
        match IncomingMessage::parse(json).unwrap() {
            IncomingMessage::ErrorResponse { id, error } => {
                assert_eq!(id, 1);
                assert_eq!(error.code, -32601);
                assert_eq!(error.message, "not found");
            }
            other => panic!("expected ErrorResponse, got: {:?}", other),
        }
    }

    #[test]
    fn parse_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///tmp/test.py","diagnostics":[]}}"#;
        match IncomingMessage::parse(json).unwrap() {
            IncomingMessage::Notification { method, params } => {
                assert_eq!(method, "textDocument/publishDiagnostics");
                assert!(params["diagnostics"].is_array());
            }
            other => panic!("expected Notification, got: {:?}", other),
        }
    }

    #[test]
    fn parse_server_request() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"workspace/configuration","params":{"items":[]}}"#;
        match IncomingMessage::parse(json).unwrap() {
            IncomingMessage::Request { id, method, params } => {
                assert_eq!(id, 1);
                assert_eq!(method, "workspace/configuration");
                assert!(params["items"].is_array());
            }
            other => panic!("expected Request, got: {:?}", other),
        }
    }

    // ===== Request ID generator =====

    #[test]
    fn request_id_unique() {
        let id_gen = RequestIdGenerator::new();
        let ids: Vec<u64> = (0..100).map(|_| id_gen.next()).collect();
        let unique: std::collections::HashSet<u64> = ids.iter().cloned().collect();
        assert_eq!(unique.len(), 100);
    }

    #[test]
    fn request_id_sequential() {
        let id_gen = RequestIdGenerator::new();
        let first = id_gen.next();
        let second = id_gen.next();
        assert_eq!(second, first + 1);
    }
}
