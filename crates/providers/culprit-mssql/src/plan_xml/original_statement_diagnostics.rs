//! Test-only structural observations while the original native SQL slice and selected XML attribute still
//! coexist. Character classes and lengths are retained; original text and value fingerprints are never emitted.
use super::*;
use culprit_core::telemetry::{MssqlParameterContextProof, StatementOrigin};
use serde::Serialize;
use serde_json::{Value, json};

const MAX_DIAGNOSTIC_BYTES: usize = 4096;

pub(super) fn report(statement: Node<'_, '_>, context: &CachedInputContext<'_>) {
    let PlanInputOrigin::OriginalCapture { source } = &context.origin else { return };
    if !MssqlParameterContextProof::supports_source(source) || !eligible_origin(context.statement.origin) {
        return;
    }
    let (Some(original), Some(selected)) = (context.text, statement.attribute("StatementText")) else { return };
    if original.trim() == selected.trim() {
        return;
    }
    let mut point = describe(original, selected);
    point["statement_origin"] = json!(context.statement.origin);
    point["statement_start_utf16_bytes"] = json!(context.statement.identity.statement_start_bytes);
    point["statement_end_utf16_bytes"] = json!(context.statement.identity.statement_end_bytes);
    point["original_source_utf16_bytes"] = json!(context.statement.source_text_bytes);
    point["original_source_text_complete"] = json!(context.statement.source_text_complete);
    let encoded = serde_json::to_string(&point).unwrap();
    assert!(encoded.len() <= MAX_DIAGNOSTIC_BYTES);
    eprintln!("owned original statement text diagnostic: {encoded}");
}

fn eligible_origin(origin: StatementOrigin) -> bool {
    !origin.is_culprit() && origin != StatementOrigin::EngineInternal
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CharacterClass {
    AsciiWhitespace,
    Letter,
    Digit,
    ParameterMarker,
    Quote,
    Bracket,
    Semicolon,
    Operator,
    Other,
}
fn class(value: char) -> CharacterClass {
    match value {
        value if value.is_ascii_whitespace() => CharacterClass::AsciiWhitespace,
        value if value.is_ascii_alphabetic() || value == '_' => CharacterClass::Letter,
        value if value.is_ascii_digit() => CharacterClass::Digit,
        '@' => CharacterClass::ParameterMarker,
        '\'' | '"' => CharacterClass::Quote,
        '[' | ']' | '(' | ')' => CharacterClass::Bracket,
        ';' => CharacterClass::Semicolon,
        '+' | '-' | '*' | '/' | '=' | '>' | '<' | '!' | '%' | '&' | '|' | '^' | '~' | '.' | ',' => {
            CharacterClass::Operator
        }
        _ => CharacterClass::Other,
    }
}

fn describe(original: &str, selected: &str) -> Value {
    let original_chars = original.chars().count();
    let selected_chars = selected.chars().count();
    let prefix = original.chars().zip(selected.chars()).take_while(|(left, right)| left == right).count();
    let suffix = original
        .chars()
        .rev()
        .zip(selected.chars().rev())
        .take_while(|(left, right)| left == right)
        .count()
        .min(original_chars.saturating_sub(prefix))
        .min(selected_chars.saturating_sub(prefix));
    let different = |text: &str, count: usize| {
        (count > prefix + suffix).then(|| {
            json!({
                "first":text.chars().nth(prefix).map(class),
                "last":text.chars().rev().nth(suffix).map(class),
            })
        })
    };
    let without_ascii_whitespace =
        |text: &str| text.chars().filter(|value| !value.is_ascii_whitespace()).collect::<String>();
    let terminal_semicolon = |text: &str| text.trim().strip_suffix(';').unwrap_or(text.trim()).to_owned();
    json!({
        "original":shape(original),"selected_xml_statement":shape(selected),
        "raw_equal":original==selected,"trim_equal":original.trim()==selected.trim(),
        "common_prefix_chars":prefix,"common_suffix_chars_without_overlap":suffix,
        "original_different_chars":original_chars-prefix-suffix,
        "selected_different_chars":selected_chars-prefix-suffix,
        "original_differing_character_classes":different(original,original_chars),
        "selected_differing_character_classes":different(selected,selected_chars),
        "equal_after_one_optional_terminal_semicolon":terminal_semicolon(original)==terminal_semicolon(selected),
        "equal_after_lossy_ascii_whitespace_removal":without_ascii_whitespace(original)==without_ascii_whitespace(selected),
        "equal_after_lossy_whitespace_and_terminal_semicolon_removal":terminal_semicolon(&without_ascii_whitespace(original))==terminal_semicolon(&without_ascii_whitespace(selected)),
        "comparison_flags_authorize_binding":false,
    })
}
fn shape(text: &str) -> Value {
    json!({
        "utf8_bytes":text.len(),"utf16_bytes":text.encode_utf16().count()*2,"chars":text.chars().count(),
        "trimmed_utf8_bytes":text.trim().len(),"ascii_whitespace_chars":text.chars().filter(|value|value.is_ascii_whitespace()).count(),
        "quote_chars":text.chars().filter(|value|matches!(value,'\''|'"')).count(),
        "semicolon_chars":text.chars().filter(|value|*value==';').count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_delimiter_is_distinguished_without_token_or_character_values() {
        let point = describe("SELECT @p;", "SELECT @p");
        assert_eq!(point["equal_after_one_optional_terminal_semicolon"], true);
        assert_eq!(point["original_differing_character_classes"]["first"], "semicolon");
        assert_eq!(point["selected_differing_character_classes"], Value::Null);
        assert_eq!(point["comparison_flags_authorize_binding"], false);
        assert_eq!(point["original_different_chars"], 1);
        assert_eq!(point["selected_different_chars"], 0);
    }
    #[test]
    fn changed_identifiers_parameters_literals_and_hints_are_not_emitted_or_declared_equivalent() {
        let left =
            "SELECT [private_table_canary],@private_parameter_canary WHERE x=N'private_literal_left' OPTION(MAXDOP 1)";
        let right = "SELECT [other_table],@other_parameter WHERE x=N'private_literal_right' OPTION(MAXDOP 2)";
        let point = describe(left, right);
        assert_eq!(point["trim_equal"], false);
        assert_eq!(point["equal_after_one_optional_terminal_semicolon"], false);
        assert_eq!(point["equal_after_lossy_ascii_whitespace_removal"], false);
        let encoded = serde_json::to_string(&point).unwrap();
        assert!(encoded.len() <= MAX_DIAGNOSTIC_BYTES);
        for secret in [
            "private_table_canary",
            "private_parameter_canary",
            "private_literal_left",
            "private_literal_right",
            "SELECT",
            "MAXDOP",
        ] {
            assert!(!encoded.contains(secret));
        }
    }
    #[test]
    fn lossy_whitespace_flags_are_explicitly_never_original_statement_authority() {
        let point = describe("SELECT N'private value'", "SELECT N'privatevalue'");
        assert_eq!(point["trim_equal"], false);
        assert_eq!(point["equal_after_lossy_ascii_whitespace_removal"], true);
        assert_eq!(point["comparison_flags_authorize_binding"], false);
        let unicode = describe("aé;", "aé");
        assert_eq!(unicode["original"]["utf8_bytes"], 4);
        assert_eq!(unicode["original"]["utf16_bytes"], 6);
        assert_eq!(unicode["common_prefix_chars"], 2);
        assert_eq!(unicode["original_different_chars"], 1);
    }
    #[test]
    fn boxed_cache_unknown_origin_does_not_suppress_the_selected_original_witness() {
        let row = crate::resources::test_support::replace(
            &crate::retained_definitions::tests::tvp_row(),
            &[("engine_internal", crate::value::Val::Int(0)), ("cloud_internal", crate::value::Val::Null)],
        );
        let context = crate::cached_statements::context(&row).unwrap();
        assert_eq!(context.origin, StatementOrigin::Unknown);
        assert!(eligible_origin(context.origin));
        assert!(!eligible_origin(StatementOrigin::CulpritMonitoring));
        assert!(!eligible_origin(StatementOrigin::Culprit));
        assert!(!eligible_origin(StatementOrigin::EngineInternal));
    }
}
