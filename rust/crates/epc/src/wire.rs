//! EPC wire protocol: 6-byte hex length framing.
//!
//! The wire format is:
//! ```text
//! [6-char hex length][UTF-8 S-expression message]\n
//! ```
//!
//! The length is the byte length of the S-expression message (including the trailing newline).
//! The hex is zero-padded to 6 characters (e.g., `00003f` for 63 bytes).
//!
//! This matches the Elisp implementation in `lsp-bridge-epc.el`:
//! ```elisp
//! (format "%06x" (length msg))
//! (encode-coding-string (concat sexp "\n") 'utf-8-unix)
//! ```

use bytes::{Buf, BufMut, BytesMut};
use thiserror::Error;

/// Errors in wire protocol framing.
#[derive(Debug, Error)]
pub enum WireError {
    #[error("incomplete frame: need {needed} more bytes")]
    Incomplete { needed: usize },
    #[error("invalid hex length: {0}")]
    InvalidHexLength(String),
    #[error("frame too large: {0} bytes")]
    FrameTooLarge(usize),
    #[error("invalid UTF-8 in frame")]
    InvalidUtf8,
}

/// Maximum frame size: 16 MB (24-bit hex = 0xFFFFFF = 16,777,215)
pub const MAX_FRAME_SIZE: usize = 0xFFFFFF;

/// Header size: 6 hex characters
pub const HEADER_SIZE: usize = 6;

/// Encode a message into wire format.
///
/// Produces: `{6-char hex length}{message}\n`
///
/// The length includes the trailing newline.
pub fn encode(message: &str) -> Vec<u8> {
    // The message on the wire is: sexp + "\n"
    let payload = format!("{}\n", message);
    let payload_bytes = payload.as_bytes();
    let len = payload_bytes.len();

    let mut buf = Vec::with_capacity(HEADER_SIZE + len);
    // Write 6-char zero-padded hex length
    let header = format!("{:06x}", len);
    buf.extend_from_slice(header.as_bytes());
    buf.extend_from_slice(payload_bytes);
    buf
}

/// Encode a message into a BytesMut buffer.
pub fn encode_into(message: &str, buf: &mut BytesMut) {
    let payload = format!("{}\n", message);
    let payload_bytes = payload.as_bytes();
    let len = payload_bytes.len();

    buf.reserve(HEADER_SIZE + len);
    let header = format!("{:06x}", len);
    buf.put_slice(header.as_bytes());
    buf.put_slice(payload_bytes);
}

/// Result of attempting to decode a frame from a buffer.
#[derive(Debug)]
pub enum DecodeResult {
    /// A complete message was decoded.
    Complete {
        /// The decoded S-expression string (without trailing newline).
        message: String,
        /// Total bytes consumed from the buffer (header + payload).
        consumed: usize,
    },
    /// Not enough data yet; need more bytes.
    Incomplete {
        /// Minimum additional bytes needed (0 if we don't know yet).
        needed: usize,
    },
}

/// Try to decode one frame from a byte buffer.
///
/// Returns `DecodeResult::Complete` with the message if a full frame is available,
/// or `DecodeResult::Incomplete` if more data is needed.
pub fn decode(buf: &[u8]) -> Result<DecodeResult, WireError> {
    // Need at least the 6-byte header
    if buf.len() < HEADER_SIZE {
        return Ok(DecodeResult::Incomplete {
            needed: HEADER_SIZE - buf.len(),
        });
    }

    // Parse hex length
    let hex_str = std::str::from_utf8(&buf[..HEADER_SIZE])
        .map_err(|_| WireError::InvalidHexLength("not ASCII".to_string()))?;

    let payload_len = usize::from_str_radix(hex_str, 16)
        .map_err(|_| WireError::InvalidHexLength(hex_str.to_string()))?;

    if payload_len > MAX_FRAME_SIZE {
        return Err(WireError::FrameTooLarge(payload_len));
    }

    let total_len = HEADER_SIZE + payload_len;

    // Check if we have the full payload
    if buf.len() < total_len {
        return Ok(DecodeResult::Incomplete {
            needed: total_len - buf.len(),
        });
    }

    // Extract payload and strip trailing newline
    let payload_bytes = &buf[HEADER_SIZE..total_len];
    let payload_str = std::str::from_utf8(payload_bytes)
        .map_err(|_| WireError::InvalidUtf8)?;

    // Strip trailing newline if present
    let message = payload_str.strip_suffix('\n').unwrap_or(payload_str);

    Ok(DecodeResult::Complete {
        message: message.to_string(),
        consumed: total_len,
    })
}

/// A streaming decoder that manages an internal buffer.
///
/// Feed data with `push()`, extract messages with `next_message()`.
pub struct FrameDecoder {
    buf: BytesMut,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self {
            buf: BytesMut::with_capacity(4096),
        }
    }

    /// Push raw bytes into the decoder buffer.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Try to extract the next complete message from the buffer.
    ///
    /// Returns `None` if no complete frame is available yet.
    pub fn next_message(&mut self) -> Result<Option<String>, WireError> {
        match decode(&self.buf)? {
            DecodeResult::Complete { message, consumed } => {
                self.buf.advance(consumed);
                Ok(Some(message))
            }
            DecodeResult::Incomplete { .. } => Ok(None),
        }
    }

    /// Returns true if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Returns the number of buffered bytes.
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }
}

impl Default for FrameDecoder {
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
    fn encode_simple_message() {
        let encoded = encode("hello");
        // "hello\n" = 6 bytes → "000006"
        assert_eq!(&encoded[..6], b"000006");
        assert_eq!(&encoded[6..], b"hello\n");
    }

    #[test]
    fn encode_empty_message() {
        let encoded = encode("");
        // "\n" = 1 byte → "000001"
        assert_eq!(&encoded[..6], b"000001");
        assert_eq!(&encoded[6..], b"\n");
    }

    #[test]
    fn encode_sexp_message() {
        let msg = r#"(call 1 open_file ("/tmp/t.py"))"#;
        let encoded = encode(msg);
        let expected_payload = format!("{}\n", msg);
        let expected_len = expected_payload.len();
        let expected_header = format!("{:06x}", expected_len);
        assert_eq!(
            std::str::from_utf8(&encoded[..6]).unwrap(),
            expected_header
        );
        assert_eq!(&encoded[6..], expected_payload.as_bytes());
    }

    #[test]
    fn encode_unicode_message() {
        let msg = "\"测试\"";
        let encoded = encode(msg);
        // "\"测试\"\n" in UTF-8: each CJK char is 3 bytes
        // \" = 1, 测 = 3, 试 = 3, \" = 1, \n = 1 → 9 bytes
        let payload = format!("{}\n", msg);
        let expected_len = payload.len();
        let expected_header = format!("{:06x}", expected_len);
        assert_eq!(
            std::str::from_utf8(&encoded[..6]).unwrap(),
            expected_header
        );
        // Verify hex counts BYTES, not characters
        assert_eq!(expected_len, 9);
        assert_eq!(
            std::str::from_utf8(&encoded[..6]).unwrap(),
            "000009"
        );
    }

    #[test]
    fn encode_large_message() {
        let msg = "x".repeat(100_000);
        let encoded = encode(&msg);
        let expected_payload = format!("{}\n", msg);
        let expected_len = expected_payload.len();
        let expected_header = format!("{:06x}", expected_len);
        assert_eq!(
            std::str::from_utf8(&encoded[..6]).unwrap(),
            expected_header
        );
    }

    // ===== Decoding tests =====

    #[test]
    fn decode_complete_frame() {
        let encoded = encode("hello");
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { message, consumed } => {
                assert_eq!(message, "hello");
                assert_eq!(consumed, encoded.len());
            }
            DecodeResult::Incomplete { .. } => panic!("expected complete"),
        }
    }

    #[test]
    fn decode_empty_message() {
        let encoded = encode("");
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { message, consumed } => {
                assert_eq!(message, "");
                assert_eq!(consumed, encoded.len());
            }
            DecodeResult::Incomplete { .. } => panic!("expected complete"),
        }
    }

    #[test]
    fn decode_sexp_message() {
        let msg = r#"(call 42 try_completion ("file.py" 10 5))"#;
        let encoded = encode(msg);
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { message, .. } => {
                assert_eq!(message, msg);
            }
            DecodeResult::Incomplete { .. } => panic!("expected complete"),
        }
    }

    #[test]
    fn decode_unicode_message() {
        let msg = "(message \"[LSP-Bridge] 测试完成\")";
        let encoded = encode(msg);
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { message, .. } => {
                assert_eq!(message, msg);
            }
            DecodeResult::Incomplete { .. } => panic!("expected complete"),
        }
    }

    #[test]
    fn decode_incomplete_header() {
        // Only 3 of 6 header bytes
        let data = b"00003";
        match decode(data).unwrap() {
            DecodeResult::Incomplete { needed } => {
                assert_eq!(needed, 1);
            }
            DecodeResult::Complete { .. } => panic!("expected incomplete"),
        }
    }

    #[test]
    fn decode_incomplete_no_data() {
        match decode(b"").unwrap() {
            DecodeResult::Incomplete { needed } => {
                assert_eq!(needed, 6);
            }
            DecodeResult::Complete { .. } => panic!("expected incomplete"),
        }
    }

    #[test]
    fn decode_incomplete_body() {
        // Header says 10 bytes but we only have 5
        let mut data = Vec::new();
        data.extend_from_slice(b"00000a"); // 10 bytes expected
        data.extend_from_slice(b"hello"); // only 5 bytes
        match decode(&data).unwrap() {
            DecodeResult::Incomplete { needed } => {
                assert_eq!(needed, 5);
            }
            DecodeResult::Complete { .. } => panic!("expected incomplete"),
        }
    }

    #[test]
    fn decode_two_frames_in_buffer() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&encode("first"));
        buf.extend_from_slice(&encode("second"));

        // Decode first
        match decode(&buf).unwrap() {
            DecodeResult::Complete {
                message, consumed, ..
            } => {
                assert_eq!(message, "first");
                // Decode second from remaining
                match decode(&buf[consumed..]).unwrap() {
                    DecodeResult::Complete { message, .. } => {
                        assert_eq!(message, "second");
                    }
                    DecodeResult::Incomplete { .. } => panic!("expected second complete"),
                }
            }
            DecodeResult::Incomplete { .. } => panic!("expected first complete"),
        }
    }

    #[test]
    fn decode_with_trailing_data() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&encode("msg"));
        buf.extend_from_slice(b"extra"); // trailing data

        match decode(&buf).unwrap() {
            DecodeResult::Complete {
                message, consumed, ..
            } => {
                assert_eq!(message, "msg");
                // Verify consumed doesn't include trailing data
                assert!(consumed < buf.len());
                assert_eq!(&buf[consumed..], b"extra");
            }
            DecodeResult::Incomplete { .. } => panic!("expected complete"),
        }
    }

    #[test]
    fn decode_invalid_hex() {
        let data = b"zzzzzzhello\n";
        assert!(decode(data).is_err());
    }

    #[test]
    fn decode_frame_too_large() {
        // MAX_FRAME_SIZE + 1: create a header claiming more bytes than allowed
        // 0xFFFFFF = 16,777,215 is max; we test with a valid 6-hex that exceeds it
        // But 6 hex chars max is 0xFFFFFF, so any valid 6-hex is within range.
        // Test with invalid (non-hex) or test behavior at boundary.
        // Actually, all 6-hex values fit in MAX_FRAME_SIZE, so this tests incomplete data instead.
        let header = format!("{:06x}", MAX_FRAME_SIZE);
        let data = header.as_bytes();
        // Should be incomplete (no payload)
        match decode(data).unwrap() {
            DecodeResult::Incomplete { .. } => {} // expected
            _ => panic!("expected incomplete for max-size frame without payload"),
        }
    }

    // ===== FrameDecoder streaming tests =====

    #[test]
    fn frame_decoder_single_message() {
        let mut decoder = FrameDecoder::new();
        let encoded = encode("hello");
        decoder.push(&encoded);

        let msg = decoder.next_message().unwrap();
        assert_eq!(msg, Some("hello".to_string()));

        // No more messages
        assert_eq!(decoder.next_message().unwrap(), None);
        assert!(decoder.is_empty());
    }

    #[test]
    fn frame_decoder_chunked_delivery() {
        let mut decoder = FrameDecoder::new();
        let encoded = encode("hello world");

        // Deliver in chunks
        let mid = encoded.len() / 2;
        decoder.push(&encoded[..mid]);
        assert_eq!(decoder.next_message().unwrap(), None); // incomplete

        decoder.push(&encoded[mid..]);
        let msg = decoder.next_message().unwrap();
        assert_eq!(msg, Some("hello world".to_string()));
    }

    #[test]
    fn frame_decoder_byte_by_byte() {
        let mut decoder = FrameDecoder::new();
        let encoded = encode("hi");

        // Deliver one byte at a time
        for (i, &byte) in encoded.iter().enumerate() {
            decoder.push(&[byte]);
            if i < encoded.len() - 1 {
                assert_eq!(decoder.next_message().unwrap(), None);
            }
        }

        let msg = decoder.next_message().unwrap();
        assert_eq!(msg, Some("hi".to_string()));
    }

    #[test]
    fn frame_decoder_multiple_messages() {
        let mut decoder = FrameDecoder::new();
        let mut buf = Vec::new();
        buf.extend_from_slice(&encode("first"));
        buf.extend_from_slice(&encode("second"));
        buf.extend_from_slice(&encode("third"));

        decoder.push(&buf);

        assert_eq!(
            decoder.next_message().unwrap(),
            Some("first".to_string())
        );
        assert_eq!(
            decoder.next_message().unwrap(),
            Some("second".to_string())
        );
        assert_eq!(
            decoder.next_message().unwrap(),
            Some("third".to_string())
        );
        assert_eq!(decoder.next_message().unwrap(), None);
    }

    #[test]
    fn frame_decoder_messages_arriving_in_bursts() {
        let mut decoder = FrameDecoder::new();

        // First message arrives complete
        decoder.push(&encode("one"));
        assert_eq!(decoder.next_message().unwrap(), Some("one".to_string()));

        // Second message arrives in two parts
        let msg2 = encode("two");
        decoder.push(&msg2[..4]);
        assert_eq!(decoder.next_message().unwrap(), None);
        decoder.push(&msg2[4..]);
        assert_eq!(decoder.next_message().unwrap(), Some("two".to_string()));

        // Third message arrives all at once
        decoder.push(&encode("three"));
        assert_eq!(
            decoder.next_message().unwrap(),
            Some("three".to_string())
        );
    }

    #[test]
    fn frame_decoder_large_payload() {
        let mut decoder = FrameDecoder::new();
        let large_msg = "x".repeat(1_000_000);
        decoder.push(&encode(&large_msg));

        let msg = decoder.next_message().unwrap();
        assert_eq!(msg.as_deref(), Some(large_msg.as_str()));
    }

    #[test]
    fn frame_decoder_unicode_heavy() {
        let mut decoder = FrameDecoder::new();
        let msg = "中文测试 日本語テスト 한국어시험 🎉🎊✨";
        decoder.push(&encode(msg));

        let decoded = decoder.next_message().unwrap();
        assert_eq!(decoded, Some(msg.to_string()));
    }

    // ===== Roundtrip: encode then decode =====

    #[test]
    fn roundtrip_simple() {
        let msg = "hello";
        let encoded = encode(msg);
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { message, .. } => assert_eq!(message, msg),
            _ => panic!("expected complete"),
        }
    }

    #[test]
    fn roundtrip_complex_sexp() {
        let msg = r#"(call 42 try_completion ("/tmp/test.py" (:line 10 :character 5) "." "os" 1))"#;
        let encoded = encode(msg);
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { message, .. } => assert_eq!(message, msg),
            _ => panic!("expected complete"),
        }
    }

    #[test]
    fn roundtrip_unicode_sexp() {
        let msg = "(message \"[LSP-Bridge] 补全完成: 测试\")";
        let encoded = encode(msg);
        match decode(&encoded).unwrap() {
            DecodeResult::Complete { message, .. } => assert_eq!(message, msg),
            _ => panic!("expected complete"),
        }
    }

    // ===== Python wire framing parity tests =====
    // Ground truth from: uv run python3 -c "format(len((sexp + '\\n').encode('utf-8')), '06x')"
    // These verify our encode() produces the exact same hex lengths as Python/Emacs.

    fn assert_wire_parity(sexp: &str, expected_hex: &str, expected_len: usize) {
        let encoded = encode(sexp);
        let hex = std::str::from_utf8(&encoded[..6]).unwrap();
        assert_eq!(
            hex, expected_hex,
            "wire hex mismatch for {:?}\n  Rust: {}\n  Python: {}",
            sexp, hex, expected_hex
        );
        let payload_len = encoded.len() - 6;
        assert_eq!(
            payload_len, expected_len,
            "wire payload len mismatch for {:?}\n  Rust: {}\n  Python: {}",
            sexp, payload_len, expected_len
        );
    }

    #[test]
    fn python_wire_eval_in_emacs_message() {
        assert_wire_parity(
            "(message '\"[LSP-Bridge] hello\")",
            "000020",
            32,
        );
    }

    #[test]
    fn python_wire_eval_in_emacs_quoted_symbol() {
        assert_wire_parity("(func 'python-mode)", "000014", 20);
    }

    #[test]
    fn python_wire_eval_in_emacs_jump() {
        assert_wire_parity(
            "(lsp-bridge-define--jump '\"/tmp/test.py\" '\"localhost\" '10 '5)",
            "00003e",
            62,
        );
    }

    #[test]
    fn python_wire_completion_record() {
        assert_wire_parity(
            "(lsp-bridge-completion--record-items '\"file.py\" '\"localhost\" '((:label \"print\" :kind \"Function\")) '(:line 10 :character 5) '\"pyright\" '(\".\") '(\"pyright\"))",
            "00009b",
            155,
        );
    }

    #[test]
    fn python_wire_integer() {
        assert_wire_parity("42", "000003", 3);
    }

    #[test]
    fn python_wire_float() {
        assert_wire_parity("3.14", "000005", 5);
    }

    #[test]
    fn python_wire_string() {
        assert_wire_parity("\"hello world\"", "00000e", 14);
    }

    #[test]
    fn python_wire_unicode() {
        assert_wire_parity("\"测试\"", "000009", 9);
    }

    #[test]
    fn python_wire_string_backslash() {
        assert_wire_parity("\"C:\\\\Users\\\\test\"", "000012", 18);
    }

    #[test]
    fn python_wire_symbol() {
        assert_wire_parity("method-name", "00000c", 12);
    }

    #[test]
    fn python_wire_epc_call() {
        assert_wire_parity(
            "(call 42 open_file (\"/tmp/test.py\"))",
            "000025",
            37,
        );
    }

    #[test]
    fn python_wire_epc_return() {
        assert_wire_parity("(return 42 \"ok\")", "000011", 17);
    }

    #[test]
    fn python_wire_epc_return_error() {
        assert_wire_parity(
            "(return-error 42 \"method not found\")",
            "000025",
            37,
        );
    }

    #[test]
    fn python_wire_epc_error() {
        assert_wire_parity(
            "(epc-error 42 \"protocol error\")",
            "000020",
            32,
        );
    }

    #[test]
    fn python_wire_epc_methods() {
        assert_wire_parity("(methods 1)", "00000c", 12);
    }

    #[test]
    fn python_wire_quoted_list() {
        assert_wire_parity("'(1 2 3)", "000009", 9);
    }
}
