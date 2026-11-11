//! Loopback HTTP fixtures consume the complete request before replying, avoiding unread-body TCP resets.
use std::{
    io::{self, Read},
    net::TcpStream,
    time::{Duration, Instant},
};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn request(socket: &mut TcpStream) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut bytes = Vec::new();
    let mut expected = None;
    let mut buffer = [0u8; 4096];
    loop {
        let remaining = deadline.checked_duration_since(Instant::now()).ok_or(io::ErrorKind::TimedOut)?;
        socket.set_read_timeout(Some(remaining))?;
        let count = socket.read(&mut buffer)?;
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if expected.is_none() {
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                if end > MAX_HEADER_BYTES {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                expected = Some(end + 4 + content_length(&bytes[..end])?);
            } else if bytes.len() > MAX_HEADER_BYTES {
                return Err(io::ErrorKind::InvalidData.into());
            }
        }
        if let Some(expected) = expected
            && bytes.len() >= expected
        {
            bytes.truncate(expected);
            return Ok(bytes);
        }
    }
}
fn content_length(bytes: &[u8]) -> io::Result<usize> {
    let text = std::str::from_utf8(bytes).map_err(|_| io::ErrorKind::InvalidData)?;
    let mut length = None;
    for line in text.lines().skip(1) {
        let (name, value) = line.split_once(':').ok_or(io::ErrorKind::InvalidData)?;
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let value = value.trim().parse::<usize>().map_err(|_| io::ErrorKind::InvalidData)?;
            if value > MAX_BODY_BYTES {
                return Err(io::ErrorKind::InvalidData.into());
            }
            length = Some(value);
        }
    }
    Ok(length.unwrap_or(0))
}
