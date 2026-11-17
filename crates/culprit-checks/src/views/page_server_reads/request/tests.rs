use super::*;

#[test]
fn request_lifetime_exact_counts_are_informational_and_never_historical_averages() {
    for pages in ["0", "9007199254740993", "9223372036854775807"] {
        let report = explain_request(pages).unwrap();
        assert_eq!(report.availability, PageServerReadAvailability::Available);
        assert_eq!(report.metrics.len(), 1);
        assert_eq!(report.metrics[0].text.as_deref(), Some(format::digits(pages.into()).as_str()));
        assert!(report.limitations.iter().any(|text| text.contains("request")));
        assert!(report.limitations.iter().any(|text| text.contains("statement")));
        assert!(!report.summary.to_lowercase().contains("cache miss"));
    }
    for invalid in ["-1", "00", "1.5", "9223372036854775808", "18446744073709551615", ""] {
        assert!(explain_request(invalid).is_err(), "{invalid}");
    }
}
