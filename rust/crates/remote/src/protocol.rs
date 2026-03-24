//! Remote file protocol — JSON message format shared between SSH and Docker.
//!
//! The protocol uses newline-delimited JSON messages.
//! Each message has a "command" field indicating the operation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Remote file sync commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command")]
pub enum SyncCommand {
    #[serde(rename = "open_file")]
    OpenFile { path: String },
    #[serde(rename = "close_file")]
    CloseFile { path: String },
    #[serde(rename = "save_file")]
    SaveFile { path: String },
    #[serde(rename = "change_file")]
    ChangeFile {
        path: String,
        #[serde(flatten)]
        args: Value,
    },
    #[serde(rename = "update_file")]
    UpdateFile { path: String, content: String },
    #[serde(rename = "remote_sync")]
    RemoteSync { info: Value },
}

/// Remote command server messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteCommand {
    pub command: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub sexp: Value,
}

/// Remote elisp RPC messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElispRpc {
    pub command: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub timestamp: String,
}

/// Encode a JSON message for the remote protocol (newline-delimited).
pub fn encode_message(value: &Value) -> Vec<u8> {
    let mut data = serde_json::to_vec(value).unwrap_or_default();
    data.push(b'\n');
    data
}

/// Decode a JSON message from a newline-delimited buffer.
pub fn decode_message(line: &str) -> Option<Value> {
    serde_json::from_str(line.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encode_decode_roundtrip() {
        let msg = json!({"command": "open_file", "path": "/tmp/test.py"});
        let encoded = encode_message(&msg);
        let line = std::str::from_utf8(&encoded).unwrap();
        let decoded = decode_message(line).unwrap();
        assert_eq!(decoded["command"], "open_file");
        assert_eq!(decoded["path"], "/tmp/test.py");
    }

    #[test]
    fn encode_has_newline() {
        let msg = json!({"test": 1});
        let encoded = encode_message(&msg);
        assert!(encoded.ends_with(b"\n"));
    }

    #[test]
    fn decode_empty_line() {
        assert!(decode_message("").is_none());
    }

    #[test]
    fn decode_invalid_json() {
        assert!(decode_message("{invalid}").is_none());
    }

    #[test]
    fn remote_command_deserialize() {
        let json = r#"{"command":"eval-in-emacs","sexp":["(message \"hello\")"]}"#;
        let cmd: RemoteCommand = serde_json::from_str(json).unwrap();
        assert_eq!(cmd.command, "eval-in-emacs");
    }

    #[test]
    fn elisp_rpc_deserialize() {
        let json = r#"{"command":"get_emacs_vars","args":["exec-path"],"timestamp":"123456"}"#;
        let rpc: ElispRpc = serde_json::from_str(json).unwrap();
        assert_eq!(rpc.command, "get_emacs_vars");
        assert_eq!(rpc.timestamp, "123456");
    }
}
