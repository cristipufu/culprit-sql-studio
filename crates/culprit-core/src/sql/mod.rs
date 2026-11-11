//! Statement-level SQL lexing for every engine: enough to tell one statement from several and reads from writes, without
//! a parser. Text from a workload or an agent is untrusted, so the rules are conservative: anything this can't lex with
//! confidence is treated as unsafe. It is a guard in front of the engines' own protections (read-only sessions and
//! transactions, prepared statements), never a replacement for them.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

mod defaults;
mod inspect;
mod lexer;
mod preview;
mod safety;
#[cfg(test)]
mod tests;

pub use defaults::{DefaultGenerator, mssql_default_generator};
pub use inspect::{
    creates_index, creates_unfiltered_index, cte_write_target, extract_call_site, has_limit, has_mssql_recompile_hint,
    isolated_counter_update, limit_count, mssql_statistics_target, offset_count, resolve_write_targets, table_aliases,
    tables_in,
};
pub use lexer::{
    LexError, block_comment_end, block_comments, block_comments_in_prefix, clean_identifier, dollar_quoted_end,
    dollar_tag, identifier_parts, line_end, quoted_end, skip_quoted_bytes, unquote_identifier,
};
pub(crate) use lexer::{balanced_parens, split_top_level_and, strip_outer_parens};
pub use preview::{ColumnSort, SortDirection, TablePreview, TablePreviewOptions, table_preview_sql};
pub use safety::{
    check, is_session_statement, mark_mssql_statements, reads_only, single_statement, without_parameter_list,
};

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum StatementProblem {
    Lexical(LexError),
    Empty,
    Multiple,
}

impl StatementProblem {
    pub fn is_unclosed(self) -> bool {
        matches!(
            self,
            Self::Lexical(LexError::UnclosedQuote | LexError::UnclosedComment | LexError::UnclosedDollarQuote)
        )
    }
}

/// What a statement does, for the question asked before it changes data on production.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct StatementCheck {
    /// One statement that only reads: it runs without asking, inside a read-only transaction where the engine has one.
    pub reads_only: bool,
    /// The leading keyword, e.g. `UPDATE`.
    pub verb: String,
    /// Why this one deserves a second look (it touches every row, or removes an object), when it does.
    pub warning: Option<String>,
    /// Why the text can't run as one statement (several statements, an unclosed quote), when it can't.
    pub problem: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub problem_kind: Option<StatementProblem>,
}
