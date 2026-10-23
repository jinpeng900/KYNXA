use std::io::{self, BufRead, Write};

use kynxa_agent_core_native::{AgentEngine, MAX_FRAME_BYTES, PROTOCOL_VERSION, Request};
use serde_json::json;

/// Bound frame reads, including inputs without a terminating newline.
/// 限制帧读取大小，缺少结束换行的输入也不能无限占用内存。
fn read_frame(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unterminated agent frame",
                ))
            };
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let consumed = end.map_or(available.len(), |index| index + 1);
        if frame.len() + consumed > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Agent frame exceeds its limit",
            ));
        }
        frame.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if end.is_some() {
            return Ok(Some(frame));
        }
    }
}

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let mut engine = AgentEngine::default();
    while let Some(frame) = read_frame(&mut reader)? {
        let response = match serde_json::from_slice::<Request>(&frame) {
            Ok(request) => engine.dispatch(request),
            Err(_) => json!({"version": PROTOCOL_VERSION, "id": null, "ok": false,
                "error": {"code": "AGENT_CORE_INVALID_FRAME", "message": "Invalid agent control frame."}}),
        };
        serde_json::to_writer(&mut writer, &response)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_and_oversized_frames_stop_before_execution() {
        assert!(read_frame(&mut io::Cursor::new(b"{\"id\":1}".as_slice())).is_err());
        assert!(read_frame(&mut io::Cursor::new(vec![b'x'; MAX_FRAME_BYTES + 1])).is_err());
        assert_eq!(
            read_frame(&mut io::Cursor::new(b"{}\n{}\n".as_slice())).unwrap(),
            Some(b"{}\n".to_vec())
        );
    }
}
