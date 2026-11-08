//! Unicode CLDR cardinal rules shared with the browser. The generated relation data is language-independent;
//! adding a catalog selects an existing language's rules without adding code or relying on the host's ICU version.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Deserialize)]
struct CardinalRules {
    languages: HashMap<String, usize>,
    rules: Vec<Vec<PluralCase>>,
}

type PluralCase = (String, Vec<Vec<Relation>>);

#[derive(Deserialize)]
struct Relation(char, u32, bool, Vec<(f64, f64)>);

fn rules() -> &'static CardinalRules {
    static RULES: OnceLock<CardinalRules> = OnceLock::new();
    RULES.get_or_init(|| {
        serde_json::from_str(include_str!("../../../../locales/.cldr/cardinals.json"))
            .expect("checked-in Unicode cardinal rules")
    })
}

/// One CLDR rule set, selected by a language tag in a catalog's `_meta.plural`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluralRule(usize);

impl PluralRule {
    pub fn for_locale(code: &str) -> Option<Self> {
        let mut tag = code;
        loop {
            if let Some(index) = rules().languages.get(tag) {
                return Some(Self(*index));
            }
            tag = tag.rsplit_once('-')?.0;
        }
    }

    pub fn category(self, number: f64) -> &'static str {
        if !number.is_finite() {
            return "other";
        }
        let operands = Operands::new(number.abs());
        rules().rules[self.0]
            .iter()
            .find(|(_, alternatives)| alternatives.iter().any(|terms| terms.iter().all(|r| r.matches(&operands))))
            .map_or("other", |(category, _)| category.as_str())
    }
}

struct Operands {
    number: f64,
    integer: f64,
    fraction_digits: f64,
    fraction: f64,
}

impl Operands {
    fn new(number: f64) -> Self {
        // Numeric arguments have no visible trailing zeroes. Expand scientific notation before computing
        // v/f; 1e-7 has seven fraction digits, even on a host that prints it with an exponent.
        let text = number.to_string();
        let (mantissa, exponent) = text
            .split_once('e')
            .map_or((text.as_str(), 0), |(m, e)| (m, e.parse::<i32>().expect("a number's exponent")));
        let whole = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
        let digits = mantissa.replace('.', "");
        let decimal = whole + exponent;
        let length = (digits.len() as i32 - decimal).max(0);
        let fraction = if length == 0 { "" } else { &digits[decimal.max(0) as usize..] };
        Self {
            number,
            integer: number.floor(),
            fraction_digits: f64::from(length),
            fraction: fraction.parse().unwrap_or(0.0),
        }
    }

    fn get(&self, name: char) -> f64 {
        match name {
            'n' => self.number,
            'i' => self.integer,
            'v' | 'w' => self.fraction_digits,
            'f' | 't' => self.fraction,
            // Culprit passes ordinary numeric values, not compact/scientific display operands.
            'c' | 'e' => 0.0,
            _ => unreachable!("the CLDR compiler accepts only defined plural operands"),
        }
    }
}

impl Relation {
    fn matches(&self, operands: &Operands) -> bool {
        let Self(operand, modulus, negate, ranges) = self;
        let mut value = operands.get(*operand);
        if *modulus != 0 {
            value %= f64::from(*modulus);
        }
        let included = value.fract() == 0.0 && ranges.iter().any(|(start, end)| value >= *start && value <= *end);
        included != *negate
    }
}
