//! Relation inventory accepts these execution hints as syntax, never as native behavior or runtime proof.
//! https://learn.microsoft.com/sql/t-sql/queries/hints-transact-sql-query
use super::{Engine, Token, expressions::MAX_EXPRESSION_DEPTH, word};
use crate::sql::lexer::literal_free_readings;

const MAX_HINTS: usize = 4;
const MAX_DOP: u32 = 32_767;
const MAX_PERCENT: u32 = 100;
const MAX_DECIMAL_DIGITS: usize = 38;

/// Return only a proved statement token scope. Original SQL and native statement text are never rewritten.
pub(super) fn statement<'a, 'sql>(text: &str, tokens: &'a [Token<'sql>]) -> Option<&'a [Token<'sql>]> {
    let end = tokens.iter().rposition(|token| !matches!(token, Token::Semicolon(_)))? + 1;
    let tokens = &tokens[..end];
    let mut depth = 0usize;
    let mut option = None;
    for (at, token) in tokens.iter().enumerate() {
        if word(Some(token), "OPTION") {
            if depth != 0 || option.replace(at).is_some() {
                return None;
            }
        }
        match token {
            Token::Punct('(') => {
                if depth == MAX_EXPRESSION_DEPTH {
                    return None;
                }
                depth += 1;
            }
            Token::Punct(')') => depth = depth.checked_sub(1)?,
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }
    let Some(at) = option else {
        return Some(tokens);
    };
    let Token::Word(option) = tokens[at] else {
        return None;
    };
    let offset = (option.as_ptr() as usize).checked_sub(text.as_ptr() as usize)?;
    // The ordinary statement lexer deliberately omits literals. The admitted hint clause must contain none,
    // so a quoted operand cannot disappear and leave an apparently valid configuration or column scope.
    literal_free_readings(text.get(offset..)?, Engine::Mssql)?;
    let tail = &tokens[at + 1..];
    if tail.first() != Some(&Token::Punct('(')) || tail.last() != Some(&Token::Punct(')')) {
        return None;
    }
    let mut seen = [false; MAX_HINTS];
    let mut count = 0usize;
    for hint in tail[1..tail.len() - 1].split(|token| *token == Token::Punct(',')) {
        let category = hint_category(text, hint)?;
        if seen[category] || count == MAX_HINTS {
            return None;
        }
        seen[category] = true;
        count += 1;
    }
    (count > 0).then_some(&tokens[..at])
}
fn hint_category(text: &str, tokens: &[Token<'_>]) -> Option<usize> {
    let Token::Word(name) = tokens.first()? else {
        return None;
    };
    if word(tokens.first(), "HASH") && tokens.len() == 2 && word(tokens.get(1), "GROUP") {
        return Some(0);
    }
    if word(tokens.first(), "MAXDOP") {
        let spelling = numeric_spelling(text, name, &tokens[1..], false)?;
        return unsigned(spelling).filter(|value| *value <= MAX_DOP).map(|_| 1);
    }
    let category = if word(tokens.first(), "MIN_GRANT_PERCENT") {
        2
    } else if word(tokens.first(), "MAX_GRANT_PERCENT") {
        3
    } else {
        return None;
    };
    if tokens.get(1) != Some(&Token::Punct('=')) {
        return None;
    }
    let spelling = numeric_spelling(text, name, &tokens[2..], true)?;
    valid_percentage(spelling).then_some(category)
}
/// Bind a decimal's exact spelling to the existing lexical tokens. Values split by whitespace, removed strings
/// or extra expressions cannot concatenate into another number. This is a numeric leaf parser, not SQL lexing.
fn numeric_spelling<'a>(text: &'a str, name: &str, tokens: &[Token<'_>], equals: bool) -> Option<&'a str> {
    let offset = (name.as_ptr() as usize).checked_sub(text.as_ptr() as usize)?.checked_add(name.len())?;
    let mut after = text.get(offset..)?.trim_ascii_start();
    if equals {
        after = after.strip_prefix('=')?.trim_ascii_start();
    }
    let end = after
        .bytes()
        .enumerate()
        .take_while(|(at, byte)| byte.is_ascii_digit() || *byte == b'.' || *at == 0 && *byte == b'+')
        .count();
    let spelling = after.get(..end)?;
    if spelling.is_empty()
        || tokens.len() != spelling.len()
        || !tokens.iter().zip(spelling.chars()).all(|(token, ch)| *token == Token::Punct(ch))
        || !matches!(after.get(end..)?.trim_ascii_start().chars().next(), Some(',' | ')'))
    {
        return None;
    }
    Some(spelling)
}
fn unsigned(value: &str) -> Option<u32> {
    let value = value.strip_prefix('+').unwrap_or(value);
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())).then(|| value.parse().ok()).flatten()
}
fn valid_percentage(value: &str) -> bool {
    let value = value.strip_prefix('+').unwrap_or(value);
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let digits = whole.len() + fraction.len();
    if digits == 0
        || digits > MAX_DECIMAL_DIGITS
        || !whole.bytes().chain(fraction.bytes()).all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    let whole = if whole.is_empty() { Some(0) } else { unsigned(whole) };
    whole
        .is_some_and(|whole| whole <= MAX_PERCENT && (whole < MAX_PERCENT || fraction.bytes().all(|byte| byte == b'0')))
}
