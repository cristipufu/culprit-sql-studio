//! Size retained projections without allocating another copy of their encoded content.
use serde::Serialize;
use std::io::{self, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonSizeError {
    LimitExceeded,
    Serialization,
}

struct CountedBytes {
    count: usize,
    limit: usize,
    exceeded: bool,
}

impl Write for CountedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.count) {
            self.exceeded = true;
            return Err(io::ErrorKind::OutOfMemory.into());
        }
        self.count += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Count compact JSON bytes, including UTF-8 and escaping, and stop at the limit. Error values never retain the
/// serialized input or a custom serializer's potentially sensitive error message.
pub fn json_size(value: &impl Serialize, limit: usize) -> Result<usize, JsonSizeError> {
    let mut writer = CountedBytes { count: 0, limit, exceeded: false };
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| if writer.exceeded { JsonSizeError::LimitExceeded } else { JsonSizeError::Serialization })?;
    Ok(writer.count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_bytes_include_utf8_escaping_and_collection_delimiters() {
        let value = serde_json::json!({"native": ["é", "query\n\"\\", "日本語"]});
        let expected = serde_json::to_vec(&value).unwrap().len();
        assert_eq!(json_size(&value, expected), Ok(expected));
        assert_eq!(json_size(&value, expected - 1), Err(JsonSizeError::LimitExceeded));
        assert_eq!(json_size(&value, 0), Err(JsonSizeError::LimitExceeded));
    }

    #[test]
    fn a_byte_limit_stops_a_streaming_serializer_before_the_full_sequence_is_visited() {
        use serde::ser::SerializeSeq;
        struct Sequence(std::cell::Cell<usize>);
        impl Serialize for Sequence {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut sequence = serializer.serialize_seq(Some(1000))?;
                for _ in 0..1000 {
                    self.0.set(self.0.get() + 1);
                    sequence.serialize_element("private-native-content")?;
                }
                sequence.end()
            }
        }
        let sequence = Sequence(std::cell::Cell::new(0));
        assert_eq!(json_size(&sequence, 16), Err(JsonSizeError::LimitExceeded));
        assert_eq!(sequence.0.get(), 1);
    }

    #[test]
    fn encoding_failures_are_distinct_from_capacity_and_do_not_expose_input() {
        struct Invalid;
        impl Serialize for Invalid {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("private-native-serialization-error-canary"))
            }
        }
        let error = json_size(&Invalid, 1024).unwrap_err();
        assert_eq!(error, JsonSizeError::Serialization);
        assert!(!format!("{error:?}").contains("canary"));
    }
}
