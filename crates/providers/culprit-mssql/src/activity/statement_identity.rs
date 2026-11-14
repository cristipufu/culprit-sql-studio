//! SQL Server statement locators are encoded once for baseline and guarded detail reads.
use super::RequestLocator;
use culprit_core::telemetry::NativeIdentity;

const OBJECT: &str = "sys.dm_exec_requests.sql_handle";
const MAX_HANDLE_LENGTH: usize = 128;

impl RequestLocator {
    pub(super) fn from_native(handle: String, start: i32, end: i32, database: i32) -> Option<Self> {
        let valid_handle = !handle.is_empty()
            && handle.len() <= MAX_HANDLE_LENGTH
            && handle.len().is_multiple_of(2)
            && handle.bytes().all(|value| value.is_ascii_hexdigit())
            && !handle.bytes().all(|value| value == b'0');
        (valid_handle
            && start >= 0
            && start % 2 == 0
            && (end == -1 || end >= start && end % 2 == 0)
            && (1..=i16::MAX as i32).contains(&database))
        .then(|| Self { handle: handle.to_ascii_uppercase(), start, end, database })
    }

    pub(super) fn identity(&self) -> NativeIdentity {
        NativeIdentity::new(&self.handle)
            .qualified("native_object", OBJECT)
            .qualified("statement_start_offset", self.start.to_string())
            .qualified("statement_end_offset", self.end.to_string())
            .qualified("database_id", self.database.to_string())
    }

    pub(super) fn from_identity(identity: &NativeIdentity) -> Option<Self> {
        if !identity.is_valid()
            || identity.qualifiers.len() != 4
            || identity.qualifiers.get("native_object").map(String::as_str) != Some(OBJECT)
        {
            return None;
        }
        let exact_integer = |name: &str| {
            let text = identity.qualifiers.get(name)?;
            let value: i32 = text.parse().ok()?;
            (value.to_string() == *text).then_some(value)
        };
        Self::from_native(
            identity.id.clone(),
            exact_integer("statement_start_offset")?,
            exact_integer("statement_end_offset")?,
            exact_integer("database_id")?,
        )
    }

    pub(super) fn matches(&self, other: &Self) -> bool {
        self.handle.eq_ignore_ascii_case(&other.handle)
            && self.start == other.start
            && self.end == other.end
            && self.database == other.database
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statement_token_roundtrips_native_database_and_byte_offsets() {
        let locator = RequestLocator::from_native("aB12".into(), 4, -1, 7).unwrap();
        let identity = locator.identity();
        assert_eq!(identity.id, "AB12");
        assert_eq!(identity.qualifiers["database_id"], "7");
        assert!(locator.matches(&RequestLocator::from_identity(&identity).unwrap()));
        for qualifier in ["database_id", "statement_start_offset", "statement_end_offset"] {
            let mut other = identity.clone();
            other
                .qualifiers
                .insert(qualifier.into(), if qualifier == "statement_start_offset" { "6" } else { "8" }.into());
            assert!(!locator.matches(&RequestLocator::from_identity(&other).unwrap()));
        }
    }

    #[test]
    fn absent_or_malformed_native_tokens_cannot_authorize_a_focused_read() {
        for (handle, start, end, database) in [
            ("", 0, -1, 7),
            ("0000", 0, -1, 7),
            ("ABC", 0, -1, 7),
            ("GG", 0, -1, 7),
            ("AB", -1, -1, 7),
            ("AB", 1, -1, 7),
            ("AB", 4, 2, 7),
            ("AB", 0, 3, 7),
            ("AB", 0, -1, 0),
            ("AB", 0, -1, i32::MAX),
        ] {
            assert!(RequestLocator::from_native(handle.into(), start, end, database).is_none());
        }
        let mut identity = RequestLocator::from_native("AB".into(), 0, -1, 7).unwrap().identity();
        identity.qualifiers.insert("unexpected".into(), "authority".into());
        assert!(RequestLocator::from_identity(&identity).is_none());
        identity.qualifiers.remove("unexpected");
        identity.qualifiers.insert("database_id".into(), "07".into());
        assert!(RequestLocator::from_identity(&identity).is_none());
    }
}
