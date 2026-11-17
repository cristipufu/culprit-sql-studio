//! Numeric rate admission only. The caller separately proves the counter population, reset and observation clock.
pub fn counter_span_seconds(reset: &crate::ResetMarker, observed_at: &str) -> Option<f64> {
    let seconds = crate::format::epoch_seconds(observed_at)? - crate::format::epoch_seconds(reset.at()?)?;
    (seconds.is_finite() && seconds >= 0.0).then_some(seconds)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdmittedCounterRate {
    total: f64,
    seconds: f64,
}

impl AdmittedCounterRate {
    pub fn new(total: f64, seconds: f64) -> Option<Self> {
        (total.is_finite() && total >= 0.0 && seconds.is_finite() && seconds >= crate::RATE_MIN_HOURS * 3600.0)
            .then_some(Self { total, seconds })
    }

    pub fn per_second(self) -> f64 {
        self.total / self.seconds
    }
    pub fn per_day(self) -> f64 {
        self.total / (self.seconds / 86400.0)
    }
    pub fn seconds(self) -> f64 {
        self.seconds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rates_require_finite_positive_span_and_nonnegative_measured_total() {
        for total in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(AdmittedCounterRate::new(total, 3600.0).is_none());
        }
        for span in [-1.0, 0.0, 3599.0, f64::NAN, f64::INFINITY] {
            assert!(AdmittedCounterRate::new(0.0, span).is_none());
        }
        let measured = AdmittedCounterRate::new(12.0, 43200.0).unwrap();
        assert_eq!(measured.per_day(), 24.0);
        assert_eq!(measured.per_second(), 12.0 / 43200.0);
        assert_eq!(AdmittedCounterRate::new(0.0, 3600.0).unwrap().per_second(), 0.0);
    }
}
