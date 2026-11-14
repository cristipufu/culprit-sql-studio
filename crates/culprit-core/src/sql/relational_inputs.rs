//! A deliberately narrow native declaration classifier, not a SQL parser or parameter-kind inference.

use super::lexer::{Token, readings};
use crate::Engine;

/// The local variable in one complete `DECLARE @name [AS] TABLE (...)` statement. Callers must additionally prove
/// that this declaration belongs to the selected statement's native batch/scope. A user-defined type name, a
/// procedure parameter, dynamic SQL or a declaration elsewhere cannot establish a local table variable here.
pub fn mssql_declared_table_variable(text: &str) -> Option<String> {
    let candidates =
        readings(text, Engine::Mssql).ok()?.iter().map(|tokens| declaration(tokens)).collect::<Option<Vec<_>>>()?;
    let first = candidates.first()?;
    candidates.iter().all(|candidate| candidate == first).then(|| first.clone())
}

/// The sole relational input of one direct SELECT, with exact native parameter spelling. Parentheses, additional
/// relations, set operations and nested statements are deliberately unqualified: a token elsewhere in a statement
/// cannot identify an expanded function's local input. Native callers must also qualify the selected plan scope.
pub fn mssql_direct_table_parameter(text: &str) -> Option<String> {
    let candidates = readings(text, Engine::Mssql)
        .ok()?
        .iter()
        .map(|tokens| direct_parameter(tokens))
        .collect::<Option<Vec<_>>>()?;
    let first = candidates.first()?;
    candidates.iter().all(|candidate| candidate == first).then(|| first.clone())
}

fn direct_parameter(tokens: &[Token<'_>]) -> Option<String> {
    let start = tokens.iter().take_while(|token| matches!(token, Token::Semicolon(_))).count();
    let end = tokens.iter().rposition(|token| !matches!(token, Token::Semicolon(_)))? + 1;
    let tokens = &tokens[start..end];
    if !matches!(tokens.first(),Some(Token::Word(word)) if word.eq_ignore_ascii_case("SELECT"))
        || tokens.iter().any(|token| matches!(token, Token::Semicolon(_) | Token::Punct('(' | ')')))
    {
        return None;
    }
    let mut from = None;
    for (at, token) in tokens.iter().enumerate() {
        let Token::Word(word) = token else { continue };
        if word.eq_ignore_ascii_case("FROM") {
            if from.replace(at).is_some() {
                return None;
            }
        } else if at > 0
            && ["SELECT", "JOIN", "APPLY", "UNION", "EXCEPT", "INTERSECT", "INTO", "FOR"]
                .iter()
                .any(|keyword| word.eq_ignore_ascii_case(keyword))
        {
            return None;
        }
    }
    let from = from?;
    let Token::Word(name) = tokens.get(from + 1)? else { return None };
    if !name.starts_with('@')
        || name.starts_with("@@")
        || name.len() < 2
        || tokens[from + 2..].contains(&Token::Punct(','))
    {
        return None;
    }
    Some((*name).into())
}

fn declaration(tokens: &[Token<'_>]) -> Option<String> {
    let start = tokens.iter().take_while(|token| matches!(token, Token::Semicolon(_))).count();
    let tokens = &tokens[start..];
    let word =
        |at: usize, value: &str| matches!(tokens.get(at), Some(Token::Word(word)) if word.eq_ignore_ascii_case(value));
    if !word(0, "DECLARE") {
        return None;
    }
    let Token::Word(name) = tokens.get(1)? else { return None };
    if !name.starts_with('@') || name.starts_with("@@") || name.len() < 2 {
        return None;
    }
    let table = if word(2, "AS") { 3 } else { 2 };
    if !word(table, "TABLE") || tokens.get(table + 1) != Some(&Token::Punct('(')) {
        return None;
    }
    let mut depth = 1usize;
    for (at, token) in tokens.iter().enumerate().skip(table + 2) {
        match token {
            Token::Punct('(') => depth += 1,
            Token::Punct(')') => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return tokens[at + 1..].iter().all(|token| matches!(token, Token::Semicolon(_))).then(|| (*name).into());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_complete_inline_local_declarations_are_identified() {
        for text in [
            "DECLARE @rows TABLE(id int)",
            "/* native declaration */ DECLARE @rows AS TABLE(id decimal(10,2),value nvarchar(10) DEFAULT N'x');",
            "DECLARE @rows TABLE([strange,column] int, CHECK ([strange,column] > 0)); -- trailing comment",
        ] {
            assert_eq!(mssql_declared_table_variable(text).as_deref(), Some("@rows"), "{text}");
        }
    }

    #[test]
    fn parameters_named_types_strings_dynamic_sql_and_ambiguous_batches_remain_unknown() {
        for text in [
            "@rows dbo.Rows READONLY",
            "DECLARE @rows dbo.Rows",
            "DECLARE @rows int",
            "EXEC(N'DECLARE @rows TABLE(id int)')",
            "SELECT 'DECLARE @rows TABLE(id int)'",
            "SELECT 1; DECLARE @rows TABLE(id int)",
            "DECLARE @rows TABLE(id int); SELECT * FROM @rows",
            "DECLARE @rows TABLE(id int), @other int",
            "DECLARE @rows TABLE(id int",
            "DECLARE @rows TABLE(id int); /* unclosed",
        ] {
            assert_eq!(mssql_declared_table_variable(text), None, "{text}");
        }
    }

    #[test]
    fn direct_parameter_scope_keeps_exact_names_and_simple_native_predicates() {
        for text in [
            "SELECT * FROM @Rows",
            "SELECT r.id,r.value FROM @Rows AS r WHERE r.id > 10;",
            "SELECT N'dbo.f(@Other)' AS label FROM @Rows -- @Other",
        ] {
            assert_eq!(mssql_direct_table_parameter(text).as_deref(), Some("@Rows"), "{text}");
        }
    }

    #[test]
    fn outer_token_occurrence_does_not_qualify_other_relations_or_expanded_function_scopes() {
        for text in [
            "SELECT * FROM dbo.expand(@Rows)",
            "SELECT r.id,dbo.expand(r.id) FROM @Rows AS r",
            "SELECT r.id FROM @Rows r CROSS APPLY dbo.expand(r.id) f",
            "SELECT * FROM @Rows r JOIN dbo.visible_view v ON v.id=r.id",
            "SELECT * FROM @Rows r, dbo.visible_view v",
            "SELECT * FROM @Rows UNION ALL SELECT * FROM dbo.visible_view",
            "SELECT * FROM @Rows WHERE id IN(SELECT id FROM dbo.visible_view)",
            "SELECT '@Rows' FROM dbo.visible_view",
            "SELECT * FROM [@Rows]",
            "SELECT * FROM \"@Rows\"",
            "EXEC(N'SELECT * FROM @Rows')",
            "SELECT * FROM @Rows;SELECT * FROM @Other",
        ] {
            assert!(mssql_direct_table_parameter(text).is_none(), "{text}");
        }
    }
}
