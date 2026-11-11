use super::*;

/// A numeric token is sign × digits × 10^exponent. Integer normalization never passes through f64.
struct Decimal {
    negative: bool,
    digits: String,
    exponent: i32,
}
impl Decimal {
    fn parse(text: &str) -> Result<Self> {
        if text.is_empty() || text.len() > 512 {
            return Err(invalid());
        }
        let (negative, unsigned) = match text.strip_prefix('-') {
            Some(value) => (true, value),
            None => (false, text),
        };
        let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
            Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().map_err(|_| invalid())?),
            None => (unsigned, 0),
        };
        if !(-400..=400).contains(&exponent) {
            return Err(invalid());
        }
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
            || mantissa.ends_with('.')
        {
            return Err(invalid());
        }
        let digits = format!("{whole}{fraction}");
        let trimmed = digits.trim_start_matches('0');
        if trimmed.is_empty() {
            return Ok(Self { negative: false, digits: "0".into(), exponent: 0 });
        }
        let significant = trimmed.trim_end_matches('0');
        let exponent = exponent - fraction.len() as i32 + (trimmed.len() - significant.len()) as i32;
        Ok(Self { negative, digits: significant.into(), exponent })
    }
}
pub(super) fn counter(text: &str) -> Result<String> {
    let decimal = Decimal::parse(text)?;
    if decimal.negative
        || decimal.exponent < 0
        || decimal.exponent > 20
        || decimal.digits.len() + decimal.exponent as usize > 20
    {
        return Err(invalid());
    }
    let text = format!("{}{}", decimal.digits, "0".repeat(decimal.exponent as usize));
    Ok(text.parse::<u64>().map_err(|_| invalid())?.to_string())
}
pub(super) fn append(value: &Value, hash: &mut Sha256) -> Result<()> {
    match value {
        Value::Null => hash.update(b"z"),
        Value::Bool(false) => hash.update(b"f"),
        Value::Bool(true) => hash.update(b"t"),
        Value::String(text) => append_string(text, hash),
        Value::Number(value) => {
            let number = Decimal::parse(&value.to_string())?;
            hash.update(b"n");
            hash.update([u8::from(number.negative)]);
            append_string(&number.digits, hash);
            hash.update(number.exponent.to_be_bytes());
        }
        Value::Array(values) => append_array(values, hash)?,
        Value::Object(values) => {
            hash.update(b"o");
            hash.update((values.len() as u64).to_be_bytes());
            let mut sorted = values.iter().collect::<Vec<_>>();
            sorted.sort_unstable_by_key(|(key, _)| *key);
            for (key, value) in sorted {
                append_string(key, hash);
                append(value, hash)?;
            }
        }
    }
    Ok(())
}
pub(super) fn append_array(values: &[Value], hash: &mut Sha256) -> Result<()> {
    hash.update(b"a");
    hash.update((values.len() as u64).to_be_bytes());
    for value in values {
        append(value, hash)?;
    }
    Ok(())
}
fn append_string(text: &str, hash: &mut Sha256) {
    hash.update(b"s");
    hash.update((text.len() as u64).to_be_bytes());
    hash.update(text.as_bytes());
}
