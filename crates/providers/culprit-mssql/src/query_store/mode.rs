//! Provider and pure settings consumers use the same native state semantics.
pub(crate) use culprit_core::engines::QueryStoreReadMode as ReadMode;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secondary_capture_is_readable_but_historical_read_only_does_not_prove_local_capture() {
        let secondary = ReadMode::from_native("READ_CAPTURE_SECONDARY").unwrap();
        assert_eq!(secondary, ReadMode::ReadCaptureSecondary);
        assert!(secondary.accepts_local_capture());
        assert!(ReadMode::from_native("READ_WRITE").unwrap().accepts_local_capture());
        assert!(!ReadMode::from_native("READ_ONLY").unwrap().accepts_local_capture());
    }

    #[test]
    fn unknown_or_unreadable_modes_cannot_gain_query_store_capability() {
        for mode in ["OFF", "ERROR", "READ_CAPTURE", "read_capture_secondary", "", "FUTURE_MODE"] {
            assert_eq!(ReadMode::from_native(mode), None, "{mode}");
        }
    }
}
