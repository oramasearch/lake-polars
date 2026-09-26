// Copyright 2026 OramaSearch.
// Licensed under the Apache License, Version 2.0.

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

use std::ops::ControlFlow;

use sqlparser::ast::{Statement, TableFactor, Visit, Visitor};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::{Parser, ParserOptions};

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

struct RelationCheck;

impl Visitor for RelationCheck {
    type Break = String;

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
    fn bad_sql_is_a_syntax_error() {
        assert!(matches!(check("SELEC 1"), Err(Refusal::Syntax(_))));
    }
}
