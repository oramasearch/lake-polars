// SPDX-License-Identifier: MIT

//! Customer SQL is untrusted code. Polars SQL can read any file the process can read through its
//! table functions (`read_csv('/etc/passwd')`, `read_parquet`, `read_json`, `read_ipc`), and its
//! non-query statements change the session's table map. Before a query reaches Polars it is
//! parsed here with the same parser, dialect and options `SQLContext::execute` uses (sqlparser
//! 0.62, `GenericDialect`, trailing commas), and refused unless:
//!
//! - it is exactly one statement, and that statement is a query (`SELECT`, `WITH`, `UNION`, …);
//! - every relation in it, however deeply nested (subqueries, CTEs, joins, `IN (SELECT …)`), is a
//!   plain table name without arguments, a derived table (a subquery), a nested join, or an
//!   `UNNEST` of literal arrays. Anything else — a table function in any spelling, a
//!   `TABLE(...)` call, `OPENJSON`, a JSON table, a pivot — is refused.
//!
//! This is an allowlist, so a table function added to a later Polars release is refused until it
//! is reviewed. Table names resolve only against the datasets registered from the data directory.
//!
//! Two more checks keep one query from taking the process down:
//!
//! - **At most [`MAX_OPERATORS`] operators** (`+`, `=`, `AND`, `OR`, `UNION`, `JOIN`, …), counted on
//!   the tokens before anything is parsed. The parser limits parenthesis nesting, but a flat chain
//!   such as `1+1+1+…` becomes a tree as deep as it is long, and the parser, this check and Polars
//!   all walk it recursively: deep enough, it overflows the stack, which aborts the process.
//! - **`lpad` and `rpad` lengths are integer literals up to [`MAX_PAD_LENGTH`]**: one line such as
//!   `lpad('a', 1000000000000)` would otherwise allocate until the process is killed.

use std::ops::ControlFlow;

use sqlparser::ast::{
    Expr, FunctionArg, FunctionArgExpr, FunctionArguments, Statement, TableFactor, Value, Visit,
    Visitor,
};
use sqlparser::dialect::GenericDialect;
use sqlparser::keywords::Keyword;
use sqlparser::parser::{Parser, ParserOptions};
use sqlparser::tokenizer::{Token, Tokenizer};

/// The most operators one query may hold (see the module documentation).
pub const MAX_OPERATORS: usize = 1_000;
/// The longest `lpad` / `rpad` result a query may ask for.
pub const MAX_PAD_LENGTH: u64 = 65_536;

/// Why a query was refused before it ran.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The SQL does not parse; the message is the parser's.
    Syntax(String),
    /// The SQL parses but is not allowed; the message says what.
    Forbidden(String),
}

/// Checks `sql` and answers `Ok(())` when it may run.
pub fn check(sql: &str) -> Result<(), Refusal> {
    let tokens = Tokenizer::new(&GenericDialect, sql)
        .tokenize()
        .map_err(|e| Refusal::Syntax(e.to_string()))?;
    let operators = tokens.iter().filter(|t| is_operator(t)).count();
    if operators > MAX_OPERATORS {
        return Err(Refusal::Forbidden(format!(
            "the query has {operators} operators; at most {MAX_OPERATORS} are allowed (use IN (...) for long lists)"
        )));
    }
    let statements = Parser::new(&GenericDialect)
        .with_options(ParserOptions {
            trailing_commas: true,
            ..Default::default()
        })
        .try_with_sql(sql)
        .and_then(|mut parser| parser.parse_statements())
        .map_err(|e| Refusal::Syntax(e.to_string()))?;
    if statements.len() != 1 {
        return Err(Refusal::Forbidden(format!(
            "send exactly one statement; found {}",
            statements.len()
        )));
    }
    let statement = &statements[0];
    if !matches!(statement, Statement::Query(_)) {
        return Err(Refusal::Forbidden(
            "only queries (SELECT, WITH, UNION, …) are allowed".to_string(),
        ));
    }
    match statement.visit(&mut RelationCheck) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(message) => Err(Refusal::Forbidden(message)),
    }
}

/// Whether a token builds one more level of expression or set-operation tree.
fn is_operator(token: &Token) -> bool {
    match token {
        Token::Word(w) => matches!(
            w.keyword,
            Keyword::AND
                | Keyword::OR
                | Keyword::XOR
                | Keyword::NOT
                | Keyword::IS
                | Keyword::LIKE
                | Keyword::ILIKE
                | Keyword::BETWEEN
                | Keyword::IN
                | Keyword::UNION
                | Keyword::EXCEPT
                | Keyword::INTERSECT
                | Keyword::JOIN
        ),
        Token::Comma
        | Token::Period
        | Token::LParen
        | Token::RParen
        | Token::SemiColon
        | Token::LBracket
        | Token::RBracket
        | Token::LBrace
        | Token::RBrace
        | Token::Whitespace(_)
        | Token::EOF
        | Token::Number(..)
        | Token::Char(_)
        | Token::SingleQuotedString(_)
        | Token::DoubleQuotedString(_)
        | Token::Placeholder(_) => false,
        // A string literal is a value; every other token is an operator (`+`, `||`, `=`, `::`, …).
        Token::NationalStringLiteral(_)
        | Token::EscapedStringLiteral(_)
        | Token::UnicodeStringLiteral(_)
        | Token::HexStringLiteral(_)
        | Token::SingleQuotedByteStringLiteral(_)
        | Token::DoubleQuotedByteStringLiteral(_)
        | Token::TripleSingleQuotedString(_)
        | Token::TripleDoubleQuotedString(_)
        | Token::DollarQuotedString(_) => false,
        _ => true,
    }
}

struct RelationCheck;

impl Visitor for RelationCheck {
    type Break = String;

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<String> {
        let Expr::Function(function) = expr else {
            return ControlFlow::Continue(());
        };
        let name = function.name.to_string().to_ascii_lowercase();
        if name != "lpad" && name != "rpad" {
            return ControlFlow::Continue(());
        }
        let length = match &function.args {
            FunctionArguments::List(list) => match list.args.get(1) {
                Some(FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(v)))) => {
                    match &v.value {
                        Value::Number(n, _) => n.parse::<u64>().ok(),
                        _ => None,
                    }
                }
                _ => None,
            },
            _ => None,
        };
        match length {
            Some(n) if n <= MAX_PAD_LENGTH => ControlFlow::Continue(()),
            _ => ControlFlow::Break(format!(
                "{name}'s length must be an integer literal up to {MAX_PAD_LENGTH}"
            )),
        }
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<String> {
        match factor {
            TableFactor::Table {
                name,
                args: Some(_),
                ..
            } => ControlFlow::Break(format!(
                "table functions are not allowed ({name}); query the uploaded datasets by name"
            )),
            TableFactor::Table { args: None, .. }
            | TableFactor::Derived { .. }
            | TableFactor::NestedJoin { .. }
            | TableFactor::UNNEST { .. } => ControlFlow::Continue(()),
            _ => ControlFlow::Break(
                "this kind of FROM item is not allowed; query the uploaded datasets by name"
                    .to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forbidden(sql: &str) -> bool {
        matches!(check(sql), Err(Refusal::Forbidden(_)))
    }

    #[test]
    fn plain_queries_pass() {
        for sql in [
            "SELECT 1",
            "SELECT * FROM a",
            "SELECT a.x, b.y FROM a JOIN b ON a.id = b.id",
            "WITH t AS (SELECT * FROM a) SELECT count(*) FROM t",
            "SELECT * FROM (SELECT * FROM a) AS s",
            "SELECT * FROM a WHERE id IN (SELECT id FROM b)",
            "SELECT x FROM a UNION ALL SELECT x FROM b",
        ] {
            assert_eq!(check(sql), Ok(()), "{sql}");
        }
    }

    #[test]
    fn file_readers_are_refused_wherever_they_appear() {
        for sql in [
            "SELECT * FROM read_csv('/etc/passwd')",
            "SELECT * FROM READ_CSV('/etc/passwd')",
            "SELECT * FROM read_parquet('/data/x.parquet')",
            "SELECT * FROM read_json('/etc/hosts')",
            "SELECT * FROM read_ipc('/etc/hosts')",
            "SELECT * FROM a JOIN read_csv('/etc/passwd') p ON true",
            "SELECT * FROM (SELECT * FROM read_csv('/etc/passwd')) s",
            "WITH t AS (SELECT * FROM read_csv('/etc/passwd')) SELECT * FROM t",
            "SELECT * FROM a WHERE x IN (SELECT * FROM read_csv('/etc/passwd'))",
            "SELECT (SELECT count(*) FROM read_csv('/etc/passwd'))",
            "SELECT * FROM a UNION SELECT * FROM read_parquet('/x')",
        ] {
            assert!(forbidden(sql), "{sql}");
        }
    }

    #[test]
    fn statements_other_than_one_query_are_refused() {
        for sql in [
            "SELECT 1; SELECT 2",
            "CREATE TABLE t AS SELECT * FROM read_csv('/etc/passwd')",
            "DROP TABLE a",
            "DELETE FROM a",
            "TRUNCATE a",
            "SHOW TABLES",
            "EXPLAIN SELECT * FROM a",
        ] {
            assert!(forbidden(sql), "{sql}");
        }
    }

    #[test]
    fn deep_operator_chains_are_refused_before_parsing() {
        let ok = format!("SELECT 1{}", "+1".repeat(MAX_OPERATORS - 1));
        assert_eq!(check(&ok), Ok(()));
        for sql in [
            format!("SELECT 1{}", "+1".repeat(MAX_OPERATORS + 1)),
            format!("SELECT 1{}", "+1".repeat(500_000)),
            format!("SELECT * FROM a WHERE x = 1{}", " OR x = 1".repeat(600)),
            format!("SELECT 1{}", " UNION SELECT 1".repeat(MAX_OPERATORS + 1)),
        ] {
            assert!(forbidden(&sql), "{}", &sql[..40]);
        }
        let list: Vec<String> = (0..20_000).map(|i| i.to_string()).collect();
        let in_list = format!("SELECT * FROM a WHERE x IN ({})", list.join(", "));
        assert_eq!(check(&in_list), Ok(()));
    }

    #[test]
    fn pad_lengths_are_bounded() {
        assert_eq!(check("SELECT lpad(name, 10, '0') FROM a"), Ok(()));
        assert_eq!(check("SELECT rpad('a', 65536)"), Ok(()));
        for sql in [
            "SELECT lpad('a', 1000000000000)",
            "SELECT RPAD('a', 65537)",
            "SELECT lpad('a', n) FROM a",
            "SELECT lpad('a', 10 * 10)",
            "SELECT * FROM a WHERE x IN (SELECT lpad('a', 99999999))",
        ] {
            assert!(forbidden(sql), "{sql}");
        }
    }

    #[test]
    fn bad_sql_is_a_syntax_error() {
        assert!(matches!(check("SELEC 1"), Err(Refusal::Syntax(_))));
    }
}
