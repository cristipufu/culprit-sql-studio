//! Complete bounded serialization precedes stdout writes, so an output-limit failure never emits partial JSON.
use culprit_core::{Result, t};
use serde::Serialize;
use std::io::{self, Write};

pub(crate) const DEFAULT_JSON_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_JSON_BYTES: usize = 256 * 1024 * 1024;

pub(crate) fn validate_json_limit(limit: usize) -> Result<()> {
    if !(1024..=MAX_JSON_BYTES).contains(&limit) {
        return Err(output_limit());
    }
    Ok(())
}

pub(crate) fn json(value: &impl Serialize, limit: usize) -> Result<Vec<u8>> {
    struct Buffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes
                .len()
                .checked_add(bytes.len())
                .filter(|size| *size <= self.limit)
                .ok_or(io::ErrorKind::OutOfMemory)?;
            self.bytes.try_reserve(bytes.len()).map_err(|_| io::ErrorKind::OutOfMemory)?;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    if limit == 0 || limit > MAX_JSON_BYTES {
        return Err(output_limit());
    }
    let mut buffer = Buffer { bytes: Vec::new(), limit };
    serde_json::to_writer(&mut buffer, value).map_err(|_| output_limit())?;
    buffer.write_all(b"\n").map_err(|_| output_limit())?;
    Ok(buffer.bytes)
}
fn output_limit() -> culprit_core::Error {
    culprit_core::Error::invalid(t!("evidence.cli.output_limit"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_output_limit_includes_final_newline_without_leaking_native_documents_on_failure() {
        let value = serde_json::json!({"native":"private-native-document-canary"});
        let expected = format!("{}\n", serde_json::to_string(&value).unwrap()).into_bytes();
        assert_eq!(json(&value, expected.len()).unwrap(), expected);
        let error = json(&value, expected.len() - 1).unwrap_err();
        assert!(!error.to_string().contains("canary"));
        for invalid in [0, MAX_JSON_BYTES + 1] {
            assert!(json(&value, invalid).is_err());
            assert!(validate_json_limit(invalid).is_err());
        }
    }
}
