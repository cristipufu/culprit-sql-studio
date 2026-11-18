//! Native integer text is compared transiently under an exact declaration/range; no literals or hashes escape.

pub(super) fn equal(data_type: Option<&str>, compiled: Option<&str>, runtime: Option<&str>) -> Option<bool> {
    let range = IntegerRange::declared(data_type?)?;
    Some(range.value(compiled?)? == range.value(runtime?)?)
}

pub(super) fn supports(data_type: Option<&str>) -> bool {
    data_type.is_some_and(|data_type| IntegerRange::declared(data_type).is_some())
}

struct IntegerRange {
    minimum: i64,
    maximum: i64,
}
impl IntegerRange {
    fn declared(data_type: &str) -> Option<Self> {
        let (minimum, maximum) = if data_type.eq_ignore_ascii_case("tinyint") {
            (0, i64::from(u8::MAX))
        } else if data_type.eq_ignore_ascii_case("smallint") {
            (i64::from(i16::MIN), i64::from(i16::MAX))
        } else if data_type.eq_ignore_ascii_case("int") {
            (i64::from(i32::MIN), i64::from(i32::MAX))
        } else if data_type.eq_ignore_ascii_case("bigint") {
            (i64::MIN, i64::MAX)
        } else {
            return None;
        };
        Some(Self { minimum, maximum })
    }
    fn value(&self, value: &str) -> Option<i64> {
        let value = trim_native(value);
        let value = match value.strip_prefix('(') {
            Some(inner) => trim_native(inner.strip_suffix(')')?),
            None => value,
        };
        let digits = value.strip_prefix('+').or_else(|| value.strip_prefix('-')).unwrap_or(value);
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let value = value.parse::<i64>().ok()?;
        (self.minimum..=self.maximum).contains(&value).then_some(value)
    }
}

fn trim_native(value: &str) -> &str {
    value.trim_matches(|ch: char| ch.is_ascii_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_integer_values_can_agree_when_their_native_representations_differ() {
        for (compiled, runtime) in
            [("(01)", "(+1)"), ("-0", "(0)"), ("( -01 )", "-1"), ("9223372036854775807", "(+9223372036854775807)")]
        {
            assert_eq!(equal(Some("bigint"), Some(compiled), Some(runtime)), Some(true));
        }
        assert_eq!(equal(Some("int"), Some("(1)"), Some("(2)")), Some(false));
        assert_eq!(equal(Some("int"), Some("(2147483647)"), Some("(-2147483648)")), Some(false));
    }
    #[test]
    fn unknown_types_ranges_expressions_or_missing_values_never_acquire_numeric_equivalence() {
        for (data_type, compiled, runtime) in [
            ("tinyint", "-1", "1"),
            ("tinyint", "256", "1"),
            ("smallint", "32768", "1"),
            ("int", "2147483648", "1"),
            ("bigint", "9223372036854775808", "1"),
            ("int", "1e0", "1"),
            ("int", "0x01", "1"),
            ("int", "N'1'", "1"),
            ("int", "(1+0)", "1"),
            ("int", "((1))", "1"),
            ("int", "(NULL)", "0"),
            ("int", "(1", "1"),
            ("int", "1)", "1"),
            ("int", "++1", "1"),
            ("int", "", "1"),
            ("int", "\u{2003}1", "1"),
            ("[int]", "1", "1"),
            ("dbo.IntAlias", "1", "1"),
            ("decimal(10,0)", "1", "1"),
            ("varchar(10)", "01", "1"),
            ("float", "1", "1"),
        ] {
            assert_eq!(
                equal(Some(data_type), Some(compiled), Some(runtime)),
                None,
                "{data_type}: {compiled}/{runtime}"
            );
        }
        assert_eq!(equal(None, Some("1"), Some("1")), None);
        assert_eq!(equal(Some("int"), None, Some("1")), None);
        assert_eq!(equal(Some("int"), Some("1"), None), None);
    }
}
