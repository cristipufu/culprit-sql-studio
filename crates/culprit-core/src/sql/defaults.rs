//! A direct native default can establish its generator; a substring or compound expression cannot.
use super::lexer::{Token, literal_free_readings};
use crate::Engine;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum DefaultGenerator {
    Sequence,
    SequentialGuid,
    Clock,
}

pub fn mssql_default_generator(expression: &str) -> Option<DefaultGenerator> {
    let readings = literal_free_readings(expression, Engine::Mssql)?;
    let mut agreed = None;
    for tokens in readings {
        let generator = classify(unwrap_parentheses(&tokens)?)?;
        if agreed.is_some_and(|before| before != generator) {
            return None;
        }
        agreed = Some(generator);
    }
    agreed
}
fn unwrap_parentheses<'a, 's>(mut tokens: &'a [Token<'s>]) -> Option<&'a [Token<'s>]> {
    while tokens.first() == Some(&Token::Punct('(')) {
        let mut depth = 0_u32;
        let mut closes = None;
        for (index, token) in tokens.iter().enumerate() {
            match token {
                Token::Punct('(') => depth += 1,
                Token::Punct(')') => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        closes = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        if closes != Some(tokens.len().checked_sub(1)?) {
            return None;
        }
        tokens = &tokens[1..tokens.len() - 1];
    }
    Some(tokens)
}
fn classify(tokens: &[Token<'_>]) -> Option<DefaultGenerator> {
    if let [Token::Word(name), Token::Punct('('), Token::Punct(')')] = tokens {
        if name.eq_ignore_ascii_case("newsequentialid") {
            return Some(DefaultGenerator::SequentialGuid);
        }
        if ["getdate", "getutcdate", "sysdatetime", "sysutcdatetime", "sysdatetimeoffset"]
            .iter()
            .any(|known| name.eq_ignore_ascii_case(known))
        {
            return Some(DefaultGenerator::Clock);
        }
    }
    if let [Token::Word(name)] = tokens
        && name.eq_ignore_ascii_case("current_timestamp")
    {
        return Some(DefaultGenerator::Clock);
    }
    let [Token::Word(next), Token::Word(value), Token::Word(for_word), name @ ..] = tokens else {
        return None;
    };
    if !next.eq_ignore_ascii_case("next")
        || !value.eq_ignore_ascii_case("value")
        || !for_word.eq_ignore_ascii_case("for")
    {
        return None;
    }
    if name.is_empty() || name.len() > 5 || name.len() % 2 == 0 {
        return None;
    }
    let valid = name.iter().enumerate().all(|(index, token)| {
        if index % 2 == 1 {
            return token == &Token::Punct('.');
        }
        match token {
            Token::Word(word) => !word.starts_with(['@', '#']),
            Token::Quoted(_, b']') => true,
            _ => false,
        }
    });
    valid.then_some(DefaultGenerator::Sequence)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_defaults_keep_comments_parentheses_and_quoted_sequence_names() {
        for (expression, kind) in [
            ("((NEXT /* source */ VALUE FOR [dbo].[Sequence.With.Dot]))", DefaultGenerator::Sequence),
            ("(newsequentialid())", DefaultGenerator::SequentialGuid),
            ("(GETUTCDATE(/* O'Neill's clock */))", DefaultGenerator::Clock),
            ("((CURRENT_TIMESTAMP))", DefaultGenerator::Clock),
        ] {
            assert_eq!(mssql_default_generator(expression), Some(kind), "{expression}");
        }
    }
    #[test]
    fn literals_user_functions_compound_expressions_and_ambiguous_names_are_unknown() {
        for expression in [
            "N'newsequentialid()'",
            "'NEXT VALUE FOR dbo.seq'",
            "getdate('ignored')",
            "getdate()+1",
            "coalesce(getdate(),getdate())",
            "dbo.getdate()",
            "[getdate]()",
            "NEXT VALUE FOR @sequence",
            "NEXT VALUE FOR seq; SELECT 1",
            "getdate() /* unfinished",
            "\"getdate\"()",
            "((getdate())",
            "NEXT VALUE FOR \"dbo\".\"sequence\"",
            "CURRENT_TIMESTAMP()",
        ] {
            assert_eq!(mssql_default_generator(expression), None, "{expression}");
        }
    }
}
