//! JSON-RPC 2.0 transport with Content-Length framing for LSP communication.
//!
//! The LSP protocol uses HTTP-style headers:
//! ```text
//! Content-Length: <byte-count>\r\n
//! \r\n
//! <JSON payload>
//! ```
//!
//! This module provides encoding and decoding for this framing format,
//! matching the Python implementation in `core/lspserver.py`.

use bytes::{Buf, BufMut, BytesMut};
use thiserror::Error;

/// Errors in JSON-RPC transport.
#[derive(Debug, Error)]
pub enum TransportError {
    #[error("incomplete message: need more data")]
    Incomplete,
    #[error("invalid header: {0}")]
    InvalidHeader(String),
    #[error("invalid JSON: {0}")]
    InvalidJson(String),
    #[error("payload too large: {0} bytes")]
    PayloadTooLarge(usize),
    #[error("invalid UTF-8")]
    InvalidUtf8,
}

/// Maximum JSON-RPC message size: 100 MB (matching Python's DEFAULT_BUFFER_SIZE)
pub const MAX_PAYLOAD_SIZE: usize = 100 * 1024 * 1024;

/// Encode a JSON-RPC message with Content-Length framing.
///
/// Produces:
/// ```text
/// Content-Length: <len>\r\n\r\n<json>
/// ```
pub fn encode(json_content: &str) -> Vec<u8> {
    let bytes = json_content.as_bytes();
    let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
    let mut buf = Vec::with_capacity(header.len() + bytes.len());
    buf.extend_from_slice(header.as_bytes());
    buf.extend_from_slice(bytes);
    buf
}

/// Encode a JSON-RPC message into a BytesMut buffer.
pub fn encode_into(json_content: &str, buf: &mut BytesMut) {
    let bytes = json_content.as_bytes();
    let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
    buf.reserve(header.len() + bytes.len());
    buf.put_slice(header.as_bytes());
    buf.put_slice(bytes);
}

/// Result of attempting to decode a Content-Length framed message.
#[derive(Debug)]
pub enum DecodeResult {
    /// A complete JSON message was decoded.
    Complete {
        /// The JSON string payload.
        json: String,
        /// Total bytes consumed from the buffer.
        consumed: usize,
    },
    /// Not enough data; need more bytes.
    Incomplete,
}

/// Try to decode one Content-Length framed message from a byte buffer.
///
/// Handles the LSP wire format:
/// - Reads `Content-Length: N\r\n\r\n` header
/// - Skips optional `Content-Type` header (dart_analysis_server compatibility)
/// - Reads exactly N bytes of JSON payload
pub fn decode(buf: &[u8]) -> Result<DecodeResult, TransportError> {
    // Search for the end of headers: \r\n\r\n
    let header_end = find_header_end(buf);
    let header_end = match header_end {
        Some(pos) => pos,
        None => return Ok(DecodeResult::Incomplete),
    };

    // Parse Content-Length from headers
    let header_bytes = &buf[..header_end];
    let header_str = std::str::from_utf8(header_bytes)
        .map_err(|_| TransportError::InvalidHeader("not UTF-8".to_string()))?;

    let content_length = parse_content_length(header_str)?;

    if content_length > MAX_PAYLOAD_SIZE {
        return Err(TransportError::PayloadTooLarge(content_length));
    }

    // header_end points to the first byte after \r\n\r\n
    let payload_start = header_end + 4; // skip \r\n\r\n
    let total_len = payload_start + content_length;

    if buf.len() < total_len {
        return Ok(DecodeResult::Incomplete);
    }

    let payload = std::str::from_utf8(&buf[payload_start..total_len])
        .map_err(|_| TransportError::InvalidUtf8)?;

    Ok(DecodeResult::Complete {
        json: payload.to_string(),
        consumed: total_len,
    })
}

/// Find the position of \r\n\r\n in the buffer.
/// Returns the position of the first \r in \r\n\r\n.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    for i in 0..buf.len() - 3 {
        if buf[i] == b'\r' && buf[i + 1] == b'\n' && buf[i + 2] == b'\r' && buf[i + 3] == b'\n' {
            return Some(i);
        }
    }
    None
}

/// Parse Content-Length from HTTP-style headers.
///
/// Handles multiple headers (e.g., Content-Type), skipping unknown ones.
/// This matches the Python receiver's behavior of ignoring Content-Type
/// headers (for dart_analysis_server compatibility).
fn parse_content_length(headers: &str) -> Result<usize, TransportError> {
    for line in headers.split("\r\n") {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            let value = value.trim();
            return value
                .parse::<usize>()
                .map_err(|_| TransportError::InvalidHeader(format!("invalid Content-Length: {}", value)));
        }
        // Skip Content-Type and other headers (dart_analysis_server compatibility)
    }
    Err(TransportError::InvalidHeader(
        "missing Content-Length header".to_string(),
    ))
}

/// A streaming decoder for Content-Length framed messages.
pub struct ContentLengthDecoder {
    buf: BytesMut,
}

impl ContentLengthDecoder {
    pub fn new() -> Self {
        Self {
            buf: BytesMut::with_capacity(8192),
        }
    }

    /// Push raw bytes into the decoder buffer.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Try to extract the next complete JSON message.
    pub fn next_message(&mut self) -> Result<Option<String>, TransportError> {
        match decode(&self.buf)? {
            DecodeResult::Complete { json, consumed } => {
                self.buf.advance(consumed);
                Ok(Some(json))
            }
            DecodeResult::Incomplete => Ok(None),
        }
    }

    /// Returns the number of buffered bytes.
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }
}

impl Default for ContentLengthDecoder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ===== Encoding tests =====

    #[test]
    fn encode_simple_request() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let encoded = encode(json);
        let expected = format!("Content-Length: {}\r\n\r\n{}", json.len(), json);
        assert_eq!(std::str::from_utf8(&encoded).unwrap(), expected);
    }

    #[test]
    fn encode_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#;
        let encoded = encode(json);
        let header = format!("Content-Length: {}\r\n\r\n", json.len());
        assert!(encoded.starts_with(header.as_bytes()));
    }

    #[test]
    fn encode_unicode_content() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"contents":"测试"}}"#;
        let encoded = encode(json);
        // Content-Length must be BYTE count, not char count
        let byte_len = json.as_bytes().len();
        let expected_header = format!("Content-Length: {}\r\n\r\n", byte_len);
        assert!(encoded.starts_with(expected_header.as_bytes()));
        // Verify the total length
        assert_eq!(encoded.len(), expected_header.len() + byte_len);
    }

    // ===== Decoding tests =====

    #[test]
    fn decode_single_message() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":"ok"}"#;
        let encoded = encode(json);
        match decode(&encoded).unwrap() {
            DecodeResult::Complete {
                json: decoded,
                consumed,
            } => {
                assert_eq!(decoded, json);
                assert_eq!(consumed, encoded.len());
            }
            DecodeResult::Incomplete => panic!("expected complete"),
        }
    }

    #[test]
    fn decode_with_content_type() {
        // dart_analysis_server sends Content-Type header
        let json = r#"{"jsonrpc":"2.0","id":1,"result":"ok"}"#;
        let frame = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}",
            json.len(),
            json
        );
        match decode(frame.as_bytes()).unwrap() {
            DecodeResult::Complete { json: decoded, .. } => {
                assert_eq!(decoded, json);
            }
            DecodeResult::Incomplete => panic!("expected complete"),
        }
    }

    #[test]
    fn decode_incomplete_header() {
        let data = b"Content-Len";
        match decode(data).unwrap() {
            DecodeResult::Incomplete => {} // expected
            _ => panic!("expected incomplete"),
        }
    }

    #[test]
    fn decode_incomplete_body() {
        let json = r#"{"jsonrpc":"2.0"}"#;
        let header = format!("Content-Length: {}\r\n\r\n", json.len());
        // Only send header + partial body
        let mut data = header.as_bytes().to_vec();
        data.extend_from_slice(&json.as_bytes()[..5]);
        match decode(&data).unwrap() {
            DecodeResult::Incomplete => {} // expected
            _ => panic!("expected incomplete"),
        }
    }

    #[test]
    fn decode_multiple_messages() {
        let json1 = r#"{"jsonrpc":"2.0","id":1,"result":"first"}"#;
        let json2 = r#"{"jsonrpc":"2.0","id":2,"result":"second"}"#;
        let mut buf = Vec::new();
        buf.extend_from_slice(&encode(json1));
        buf.extend_from_slice(&encode(json2));

        // Decode first
        match decode(&buf).unwrap() {
            DecodeResult::Complete {
                json, consumed, ..
            } => {
                assert_eq!(json, json1);
                // Decode second
                match decode(&buf[consumed..]).unwrap() {
                    DecodeResult::Complete { json, .. } => {
                        assert_eq!(json, json2);
                    }
                    DecodeResult::Incomplete => panic!("expected second complete"),
                }
            }
            DecodeResult::Incomplete => panic!("expected first complete"),
        }
    }

    #[test]
    fn decode_unicode_payload() {
        let json = r#"{"result":"测试中文"}"#;
        let encoded = encode(json);
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { json: decoded, .. } => {
                assert_eq!(decoded, json);
            }
            DecodeResult::Incomplete => panic!("expected complete"),
        }
    }

    #[test]
    fn decode_missing_content_length() {
        let data = b"Content-Type: text/plain\r\n\r\n{}";
        assert!(decode(data).is_err());
    }

    // ===== ContentLengthDecoder streaming tests =====

    #[test]
    fn decoder_single_message() {
        let mut decoder = ContentLengthDecoder::new();
        let json = r#"{"jsonrpc":"2.0","id":1}"#;
        decoder.push(&encode(json));

        let msg = decoder.next_message().unwrap();
        assert_eq!(msg, Some(json.to_string()));
        assert_eq!(decoder.next_message().unwrap(), None);
    }

    #[test]
    fn decoder_chunked_delivery() {
        let mut decoder = ContentLengthDecoder::new();
        let json = r#"{"jsonrpc":"2.0","id":1,"result":"hello world"}"#;
        let encoded = encode(json);

        // Deliver header first, then body
        let mid = encoded.len() / 2;
        decoder.push(&encoded[..mid]);
        assert_eq!(decoder.next_message().unwrap(), None);

        decoder.push(&encoded[mid..]);
        assert_eq!(decoder.next_message().unwrap(), Some(json.to_string()));
    }

    #[test]
    fn decoder_byte_by_byte() {
        let mut decoder = ContentLengthDecoder::new();
        let json = r#"{"id":1}"#;
        let encoded = encode(json);

        for (i, &byte) in encoded.iter().enumerate() {
            decoder.push(&[byte]);
            if i < encoded.len() - 1 {
                assert_eq!(decoder.next_message().unwrap(), None);
            }
        }
        assert_eq!(decoder.next_message().unwrap(), Some(json.to_string()));
    }

    #[test]
    fn decoder_multiple_messages() {
        let mut decoder = ContentLengthDecoder::new();
        let msgs = vec![
            r#"{"jsonrpc":"2.0","id":1}"#,
            r#"{"jsonrpc":"2.0","id":2}"#,
            r#"{"jsonrpc":"2.0","id":3}"#,
        ];

        let mut buf = Vec::new();
        for msg in &msgs {
            buf.extend_from_slice(&encode(msg));
        }
        decoder.push(&buf);

        for expected in &msgs {
            assert_eq!(
                decoder.next_message().unwrap(),
                Some(expected.to_string())
            );
        }
        assert_eq!(decoder.next_message().unwrap(), None);
    }

    #[test]
    fn decoder_with_content_type_header() {
        let mut decoder = ContentLengthDecoder::new();
        let json = r#"{"id":1}"#;
        let frame = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}",
            json.len(),
            json
        );
        decoder.push(frame.as_bytes());

        assert_eq!(decoder.next_message().unwrap(), Some(json.to_string()));
    }

    // ===== Roundtrip tests =====

    #[test]
    fn roundtrip_request() {
        let json = r#"{"jsonrpc":"2.0","id":42001,"method":"textDocument/completion","params":{"textDocument":{"uri":"file:///tmp/test.py"},"position":{"line":10,"character":5}}}"#;
        let encoded = encode(json);
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { json: decoded, .. } => {
                assert_eq!(decoded, json);
            }
            _ => panic!("expected complete"),
        }
    }

    #[test]
    fn roundtrip_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///tmp/test.py","version":1},"contentChanges":[{"text":"import os\n"}]}}"#;
        let encoded = encode(json);
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { json: decoded, .. } => {
                assert_eq!(decoded, json);
            }
            _ => panic!("expected complete"),
        }
    }

    #[test]
    fn roundtrip_response() {
        let json = r#"{"jsonrpc":"2.0","id":42001,"result":{"items":[{"label":"print","kind":3}]}}"#;
        let encoded = encode(json);
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { json: decoded, .. } => {
                assert_eq!(decoded, json);
            }
            _ => panic!("expected complete"),
        }
    }

    #[test]
    fn roundtrip_error_response() {
        let json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found"}}"#;
        let encoded = encode(json);
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { json: decoded, .. } => {
                assert_eq!(decoded, json);
            }
            _ => panic!("expected complete"),
        }
    }

    #[test]
    fn roundtrip_unicode_response() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"contents":{"kind":"markdown","value":"```python\ndef 测试(): ...\n```"}}}"#;
        let encoded = encode(json);
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { json: decoded, .. } => {
                assert_eq!(decoded, json);
            }
            _ => panic!("expected complete"),
        }
    }

    // ===== Python ground truth tests =====
    // Content-Length values generated by Python: len(json.dumps(msg).encode('utf-8'))

    fn assert_content_length_parity(json: &str, expected_len: usize) {
        let byte_len = json.as_bytes().len();
        assert_eq!(
            byte_len, expected_len,
            "Content-Length mismatch for JSON:\n  Rust byte len: {}\n  Python expected: {}",
            byte_len, expected_len
        );
        // Also verify encode produces correct header
        let encoded = encode(json);
        let header = format!("Content-Length: {}\r\n\r\n", expected_len);
        assert!(
            encoded.starts_with(header.as_bytes()),
            "Header mismatch. Expected: {:?}",
            header
        );
    }

    #[test]
    fn python_parity_initialize_request() {
        let json = r#"{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"processId": 1234}}"#;
        assert_content_length_parity(json, 82);
    }

    #[test]
    fn python_parity_initialized_notification() {
        let json = r#"{"jsonrpc": "2.0", "method": "initialized", "params": {}}"#;
        assert_content_length_parity(json, 57);
    }

    #[test]
    fn python_parity_did_open() {
        let json = r#"{"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": "file:///tmp/test.py", "languageId": "python", "version": 0, "text": "import os\n"}}}"#;
        assert_content_length_parity(json, 173);
    }

    #[test]
    fn python_parity_completion_request() {
        let json = r#"{"jsonrpc": "2.0", "id": 42001, "method": "textDocument/completion", "params": {"textDocument": {"uri": "file:///tmp/test.py"}, "position": {"line": 1, "character": 3}, "context": {"triggerKind": 2, "triggerCharacter": "."}}}"#;
        assert_content_length_parity(json, 225);
    }

    #[test]
    fn python_parity_completion_response() {
        let json = r#"{"jsonrpc": "2.0", "id": 42001, "result": {"items": [{"label": "system", "kind": 3, "detail": "(command: str) -> int", "sortText": "0system"}, {"label": "path", "kind": 9, "detail": "module", "sortText": "0path"}]}}"#;
        assert_content_length_parity(json, 215);
    }

    #[test]
    fn python_parity_error_response() {
        let json = r#"{"jsonrpc": "2.0", "id": 1, "error": {"code": -32601, "message": "method not found"}}"#;
        assert_content_length_parity(json, 85);
    }

    #[test]
    fn python_parity_diagnostics() {
        let json = r#"{"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": "file:///tmp/test.py", "diagnostics": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 9}}, "severity": 2, "source": "pyright", "message": "Import \"os\" is not used"}]}}"#;
        assert_content_length_parity(json, 285);
    }

    #[test]
    fn python_parity_did_change() {
        let json = r#"{"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {"textDocument": {"uri": "file:///tmp/test.py", "version": 1}, "contentChanges": [{"range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 0}}, "rangeLength": 0, "text": "os."}]}}"#;
        assert_content_length_parity(json, 268);
    }

    // ===== Full wire roundtrip with Python Content-Length values =====

    #[test]
    fn python_wire_roundtrip_initialize() {
        let json = r#"{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"processId": 1234}}"#;
        let encoded = encode(json);
        // Verify wire format matches Python exactly
        let expected_wire = "Content-Length: 82\r\n\r\n{\"jsonrpc\": \"2.0\", \"id\": 1, \"method\": \"initialize\", \"params\": {\"processId\": 1234}}";
        assert_eq!(std::str::from_utf8(&encoded).unwrap(), expected_wire);
        // Decode back
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { json: decoded, .. } => assert_eq!(decoded, json),
            _ => panic!("expected complete"),
        }
    }

    #[test]
    fn python_wire_roundtrip_diagnostics() {
        let json = r#"{"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": "file:///tmp/test.py", "diagnostics": [{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 9}}, "severity": 2, "source": "pyright", "message": "Import \"os\" is not used"}]}}"#;
        let encoded = encode(json);
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { json: decoded, .. } => assert_eq!(decoded, json),
            _ => panic!("expected complete"),
        }
    }

    // ===== Streaming decode with realistic LSP message sequences =====

    #[test]
    fn decoder_lsp_init_sequence() {
        let mut decoder = ContentLengthDecoder::new();

        // Simulate receiving init response + initialized notification + diagnostics
        let msgs = vec![
            r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"completionProvider":{"triggerCharacters":["."]}}}}"#,
            r#"{"jsonrpc":"2.0","method":"window/logMessage","params":{"type":3,"message":"Server initialized"}}"#,
            r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///tmp/t.py","diagnostics":[]}}"#,
        ];

        let mut buf = Vec::new();
        for msg in &msgs {
            buf.extend_from_slice(&encode(msg));
        }

        // Deliver all at once
        decoder.push(&buf);

        for expected in &msgs {
            assert_eq!(
                decoder.next_message().unwrap(),
                Some(expected.to_string())
            );
        }
        assert_eq!(decoder.next_message().unwrap(), None);
    }

    #[test]
    fn decoder_lsp_messages_chunked() {
        let mut decoder = ContentLengthDecoder::new();

        let msg1 = r#"{"jsonrpc":"2.0","id":1,"result":{"items":[{"label":"print"}]}}"#;
        let msg2 = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"diagnostics":[]}}"#;

        let mut full_buf = Vec::new();
        full_buf.extend_from_slice(&encode(msg1));
        full_buf.extend_from_slice(&encode(msg2));

        // Deliver in 10-byte chunks
        for chunk in full_buf.chunks(10) {
            decoder.push(chunk);
            // Drain any complete messages
            while let Some(_msg) = decoder.next_message().unwrap() {
                // consumed
            }
        }

        // Buffer should be empty after all chunks
        assert_eq!(decoder.buffered_len(), 0);
    }
}
