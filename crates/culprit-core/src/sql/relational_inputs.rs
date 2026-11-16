//! Strict relational-reference and declaration recognition for native SQL Server input binding.

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

mod references;
pub use references::{MssqlRelationName, MssqlRelationReference, mssql_relational_references};

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
}
