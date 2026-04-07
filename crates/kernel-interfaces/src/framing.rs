//! Length-prefixed JSON framing for IPC transport.
//!
//! Wire format: 4-byte big-endian length prefix followed by UTF-8 JSON bytes.
//! Transport-agnostic — works over any `Read`/`Write` (Unix sockets, pipes, TCP).

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{self, Read, Write};

/// Maximum message size: 64 MiB. Prevents unbounded allocation from
/// malformed length prefixes.
const MAX_MESSAGE_SIZE: u32 = 64 * 1024 * 1024;

/// Write a length-prefixed JSON message.
pub fn write_message<W: Write>(writer: &mut W, msg: &impl Serialize) -> io::Result<()> {
    let json =
        serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = json.len() as u32;
    writer.write_all(&len.to_be_bytes())?;
    writer.write_all(&json)?;
    writer.flush()?;
    Ok(())
}

/// Read a length-prefixed JSON message.
///
/// Returns `ErrorKind::UnexpectedEof` if the stream is closed cleanly.
pub fn read_message<R: Read, T: DeserializeOwned>(reader: &mut R) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);

    if len > MAX_MESSAGE_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "message too large: {} bytes (max {})",
                len, MAX_MESSAGE_SIZE
            ),
        ));
    }

    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf)?;

    serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::*;
    use crate::types::*;
    use std::io::Cursor;

    #[test]
    fn round_trip_over_buffer() {
        let msg = KernelRequest::AddInput {
            session_id: SessionId(42),
            text: "Hello, world!".into(),
        };

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).expect("write");

        let mut cursor = Cursor::new(&buf);
        let decoded: KernelRequest = read_message(&mut cursor).expect("read");

        let original_json = serde_json::to_string(&msg).unwrap();
        let decoded_json = serde_json::to_string(&decoded).unwrap();
        assert_eq!(original_json, decoded_json);
    }

    #[test]
    fn multiple_messages_on_same_stream() {
        let messages = vec![
            KernelEvent::TurnStarted {
                session_id: SessionId(0),
                turn_id: TurnId(0),
            },
            KernelEvent::TextOutput {
                session_id: SessionId(0),
                text: "Hi there".into(),
            },
            KernelEvent::TurnEnded {
                session_id: SessionId(0),
                turn_id: TurnId(0),
                result: TurnResultSummary {
                    tool_calls_dispatched: 0,
                    tool_calls_denied: 0,
                    was_cancelled: false,
                    input_tokens: 50,
                    output_tokens: 20,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
            },
        ];

        let mut buf = Vec::new();
        for msg in &messages {
            write_message(&mut buf, msg).expect("write");
        }

        let mut cursor = Cursor::new(&buf);
        for original in &messages {
            let decoded: KernelEvent = read_message(&mut cursor).expect("read");
            assert_eq!(
                serde_json::to_string(original).unwrap(),
                serde_json::to_string(&decoded).unwrap(),
            );
        }
    }

    #[test]
    fn oversized_message_rejected() {
        // Craft a buffer with a length prefix exceeding MAX_MESSAGE_SIZE
        let fake_len: u32 = MAX_MESSAGE_SIZE + 1;
        let buf = fake_len.to_be_bytes().to_vec();
        let mut cursor = Cursor::new(&buf);
        let result: io::Result<KernelRequest> = read_message(&mut cursor);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn eof_on_empty_stream() {
        let buf: Vec<u8> = Vec::new();
        let mut cursor = Cursor::new(&buf);
        let result: io::Result<KernelRequest> = read_message(&mut cursor);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn length_prefix_is_big_endian() {
        let msg = KernelRequest::Shutdown;
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).expect("write");

        // First 4 bytes are the length prefix in big-endian
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let json_bytes = &buf[4..];
        assert_eq!(len as usize, json_bytes.len());

        // Verify it's valid JSON
        let _: KernelRequest = serde_json::from_slice(json_bytes).expect("valid json");
    }
}
