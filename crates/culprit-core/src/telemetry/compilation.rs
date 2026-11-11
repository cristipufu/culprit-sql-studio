//! Retained Query Store compilation totals have their own lifetime, independent of runtime intervals.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct MssqlQueryCompilation {
    /// Native references are scoped by the capture's database, not a different profile's definition ID.
    pub query_id: String,
    pub first_compile_at: String,
    pub last_compile_at: String,
    pub compilations: String,
    pub average_duration_us: f64,
    /// The native clock comparison can be negative after clock skew; it is not a measured duration.
    pub seconds_since_first_compile: i64,
    pub reset: crate::ResetMarker,
}
impl MssqlQueryCompilation {
    pub fn key(&self) -> Result<String> {
        let first = canonical_time(&self.first_compile_at)?;
        serde_json::to_vec(&(&self.query_id, first))
            .map(|bytes| digest(&bytes))
            .map_err(|_| CollectionStop::InvalidPage.error())
    }
    pub fn is_valid(&self) -> bool {
        valid_counter(&self.query_id)
            && valid_counter(&self.compilations)
            && [&self.first_compile_at, &self.last_compile_at]
                .into_iter()
                .all(|at| at.len() <= 64 && canonical_time(at).is_ok())
            && self.average_duration_us.is_finite()
            && self.average_duration_us >= 0.0
            && valid_reset(&self.reset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_lifetime_identity_preserves_exact_counts_and_clock_skew() {
        let value = MssqlQueryCompilation {
            query_id: "3".into(),
            first_compile_at: "2020-01-01T06:00:00.1533333Z".into(),
            last_compile_at: "2020-01-01T06:01:00Z".into(),
            compilations: "9007199254740993".into(),
            average_duration_us: 1500.5,
            seconds_since_first_compile: -1,
            reset: crate::ResetMarker::Unknown,
        };
        assert!(value.is_valid());
        let mut changed = value.clone();
        changed.compilations = "9007199254740994".into();
        assert_eq!(value.key().unwrap(), changed.key().unwrap());
        changed.first_compile_at = "2020-01-01T06:00:00.153333300+00:00".into();
        assert_eq!(value.key().unwrap(), changed.key().unwrap());
        changed.first_compile_at = "2020-01-01T06:00:01Z".into();
        assert_ne!(value.key().unwrap(), changed.key().unwrap());
        changed.average_duration_us = f64::NAN;
        assert!(!changed.is_valid());
    }
}
