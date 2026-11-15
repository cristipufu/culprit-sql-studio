//! Showplan type metadata is a qualified identifier with an optional native size, never arbitrary SQL text.
use crate::{
    Engine,
    plan_facts::parameter_type_is_valid,
    sql::{identifier_parts, quoted_end},
};

pub(super) fn redact(data_type: &str) -> Option<String> {
    let data_type = data_type.trim();
    if !parameter_type_is_valid(data_type) {
        return None;
    }
    let name_end = name_end(data_type)?;
    let name = data_type[..name_end].trim();
    let parts = identifier_parts(name)?;
    let modifiers = data_type[name_end..].trim();
    if !modifiers.is_empty() && (parts.len() != 1 || !valid_modifiers(&parts[0], modifiers)) {
        return None;
    }
    let retained_name = if parts.len() > 2 {
        parts[parts.len() - 2..].iter().map(|part| Engine::Mssql.dialect().quote(part)).collect::<Vec<_>>().join(".")
    } else {
        name.to_owned()
    };
    let retained = format!("{retained_name}{modifiers}");
    parameter_type_is_valid(&retained).then_some(retained)
}

fn name_end(value: &str) -> Option<usize> {
    let mut offset = 0;
    let mut parts = 0;
    loop {
        let rest = &value[offset..];
        let first = rest.chars().next()?;
        offset = match first {
            '[' => quoted_end(value.as_bytes(), offset, b']', false).ok()?,
            '"' => quoted_end(value.as_bytes(), offset, b'"', false).ok()?,
            first if first.is_alphabetic() || matches!(first, '_' | '@' | '#') => {
                offset
                    + rest
                        .chars()
                        .take_while(|character| {
                            character.is_alphanumeric() || matches!(*character, '_' | '@' | '#' | '$')
                        })
                        .map(char::len_utf8)
                        .sum::<usize>()
            }
            _ => return None,
        };
        parts += 1;
        if parts > 4 {
            return None;
        }
        offset += value[offset..]
            .chars()
            .take_while(|character| character.is_ascii_whitespace())
            .map(char::len_utf8)
            .sum::<usize>();
        match value.as_bytes().get(offset) {
            None | Some(b'(') => return Some(offset),
            Some(b'.') => {
                offset += 1;
                offset += value[offset..]
                    .chars()
                    .take_while(|character| character.is_ascii_whitespace())
                    .map(char::len_utf8)
                    .sum::<usize>();
            }
            _ => return None,
        }
    }
}

fn valid_modifiers(name: &str, modifiers: &str) -> bool {
    if !super::super::sql::SIZED_TYPES.iter().any(|sized| sized.eq_ignore_ascii_case(name)) {
        return false;
    }
    let Some(parameters) = modifiers.strip_prefix('(').and_then(|value| value.strip_suffix(')')) else { return false };
    if parameters.trim().eq_ignore_ascii_case("max") {
        return true;
    }
    let parameters = parameters.split(',').map(str::trim).collect::<Vec<_>>();
    (1..=2).contains(&parameters.len())
        && parameters.iter().all(|value| {
            !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) && value.parse::<u32>().is_ok()
        })
}
