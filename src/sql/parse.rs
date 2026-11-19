//! Parsing a dbt model: neutralise its Jinja, then parse the result with
//! sqlparser-rs, falling back to its tokenizer when no dialect parses it.
//!
//! See docs/design/sql-models.md, decision 3. With `sql.dialect` set, that
//! dialect alone is tried. At `generic`, the default, the generic dialect is
//! tried first and then Snowflake, BigQuery, Databricks and Postgres, in that
//! order: the rules read only relation names, `USE` statements and string
//! literals, so which dialect parsed doesn't matter. A model that no dialect
//! parses, or whose Jinja can't be neutralised, is read through its tokens
//! instead and reported as `sql_not_parsed`.
//!
//! Positions carry over unchanged: the neutraliser keeps every byte offset
//! and every line break, so a token's line in the parsed text is its line in
//! the source, and [`source_position`] gives its column there.

use super::neutralise::{self, is_placeholder};
use crate::config::SqlDialect;
use sqlparser::ast::Statement;
use sqlparser::dialect::{
    BigQueryDialect, DatabricksDialect, Dialect, GenericDialect, PostgreSqlDialect,
    SnowflakeDialect,
};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Location, TokenWithSpan, Tokenizer};

/// A model as the rules read it.
pub struct Model {
    /// The neutralised text, the same length and lines as the source. When
    /// the Jinja couldn't be neutralised, the neutraliser's best effort.
    // Read by the stage-1 rules, which land in the next part.
    #[allow(dead_code)]
    pub text: String,
    pub parse: Parse,
}

pub enum Parse {
    /// Every statement parsed.
    Full {
        #[allow(dead_code)] // read by the stage-1 rules, next part
        statements: Vec<Statement>,
        /// The dialect that parsed it.
        #[allow(dead_code)] // read by the stage-1 rules, next part
        dialect: SqlDialect,
    },
    /// Not parsed: the rules read the tokens, and the file is skipped as
    /// `sql_not_parsed` with `cause` as its detail.
    Partial {
        cause: String,
        /// Empty only if the tokenizer failed too, which `cause` then says.
        #[allow(dead_code)] // read by the stage-1 rules, next part
        tokens: Vec<TokenWithSpan>,
    },
}

/// The dialects tried, in order, for `sql.dialect`.
fn dialects(pinned: SqlDialect) -> Vec<(SqlDialect, Box<dyn Dialect>)> {
    let all = [
        SqlDialect::Generic,
        SqlDialect::Snowflake,
        SqlDialect::Bigquery,
        SqlDialect::Databricks,
        SqlDialect::Postgres,
    ];
    let wanted: Vec<SqlDialect> = if pinned == SqlDialect::Generic {
        all.to_vec()
    } else {
        vec![pinned]
    };
    wanted.into_iter().map(|d| (d, dialect(d))).collect()
}

fn dialect(d: SqlDialect) -> Box<dyn Dialect> {
    match d {
        SqlDialect::Generic => Box::new(GenericDialect {}),
        SqlDialect::Snowflake => Box::new(SnowflakeDialect {}),
        SqlDialect::Bigquery => Box::new(BigQueryDialect {}),
        SqlDialect::Databricks => Box::new(DatabricksDialect {}),
        SqlDialect::Postgres => Box::new(PostgreSqlDialect {}),
    }
}

/// The name a person writes in `sql.dialect`.
pub fn dialect_name(d: SqlDialect) -> &'static str {
    match d {
        SqlDialect::Generic => "generic",
        SqlDialect::Snowflake => "snowflake",
        SqlDialect::Bigquery => "bigquery",
        SqlDialect::Databricks => "databricks",
        SqlDialect::Postgres => "postgres",
    }
}

/// Neutralise and parse `src`.
pub fn read(src: &str, pinned: SqlDialect) -> Model {
    let (text, jinja_problem) = match neutralise::neutralise(src) {
        Ok(text) => (text, None),
        Err(f) => (f.text, Some(f.reason)),
    };
    let tried = dialects(pinned);
    let mut errors: Vec<(SqlDialect, sqlparser::parser::ParserError)> = Vec::new();
    if jinja_problem.is_none() {
        for (name, d) in &tried {
            match Parser::parse_sql(d.as_ref(), &text) {
                Ok(statements) => {
                    return Model {
                        text,
                        parse: Parse::Full {
                            statements,
                            dialect: *name,
                        },
                    }
                }
                Err(e) => errors.push((*name, e)),
            }
        }
    }
    let (first, fallback) = &tried[0];
    let (tokens, tokenizer_error) =
        match Tokenizer::new(fallback.as_ref(), &text).tokenize_with_location() {
            Ok(tokens) => (tokens, None),
            Err(e) => (Vec::new(), Some(e.to_string())),
        };
    let mut cause = match jinja_problem {
        Some(reason) => format!("its Jinja can't be neutralised: {reason}"),
        None => no_dialect_parses(pinned, &errors),
    };
    match tokenizer_error {
        Some(e) => cause.push_str(&format!(
            "; the {} tokenizer failed too ({}), so nothing in it was checked",
            dialect_name(*first),
            readable(&e)
        )),
        None => cause.push_str("; checked from its tokens, so some findings may be missed"),
    }
    Model {
        text,
        parse: Parse::Partial { cause, tokens },
    }
}

/// Why no dialect parsed the text: the error that got furthest into the
/// file, since the dialect that read the most is the likeliest to name the
/// real obstacle.
fn no_dialect_parses(
    pinned: SqlDialect,
    errors: &[(SqlDialect, sqlparser::parser::ParserError)],
) -> String {
    // `max_by_key` keeps the last of equals; reversed, that is the first
    // dialect tried, so a tie names the generic dialect.
    let furthest = errors
        .iter()
        .rev()
        .map(|(d, e)| (d, e.to_string()))
        .max_by_key(|(_, e)| position(e).unwrap_or((0, 0)))
        .expect("at least one dialect was tried");
    let (d, e) = furthest;
    if pinned == SqlDialect::Generic {
        format!(
            "no dialect parses it ({}: {})",
            dialect_name(*d),
            readable(&e)
        )
    } else {
        format!(
            "the {} dialect (sql.dialect) doesn't parse it: {}",
            dialect_name(*d),
            readable(&e)
        )
    }
}

/// `(line, column)` from sqlparser-rs's "at Line: L, Column: C" suffix.
fn position(error: &str) -> Option<(u64, u64)> {
    let at = error.rfind(" at Line: ")?;
    let rest = &error[at + " at Line: ".len()..];
    let (line, column) = rest.split_once(", Column: ")?;
    Some((line.trim().parse().ok()?, column.trim().parse().ok()?))
}

/// An error as a person reads it: "line 95, column 9: expected `)`, found a
/// Jinja expression" rather than sqlparser-rs's raw text. A placeholder is
/// named for what it stands for.
fn readable(error: &str) -> String {
    let body = error
        .strip_prefix("sql parser error: ")
        .or_else(|| error.strip_prefix("sql tokenizer error: "))
        .unwrap_or(error);
    let (body, at) = match body.rfind(" at Line: ") {
        Some(i) => (&body[..i], position(body)),
        None => (body, None),
    };
    let body = match body
        .strip_prefix("Expected: ")
        .and_then(|r| r.split_once(", found: "))
    {
        Some((expected, found)) => {
            let found = if is_placeholder(found) {
                "a Jinja expression".to_string()
            } else if found == "EOF" {
                "the end of the file".to_string()
            } else {
                format!("`{found}`")
            };
            format!("expected {expected}, found {found}")
        }
        None => body.to_string(),
    };
    match at {
        Some((line, column)) => format!("line {line}, column {column}: {body}"),
        None => body,
    }
}

/// The byte offset of `loc` (1-based line and column, columns counted in
/// characters, as sqlparser-rs counts them) in `text`.
#[allow(dead_code)] // used by the stage-1 rules, next part
pub fn offset(text: &str, loc: Location) -> usize {
    let line_start = if loc.line <= 1 {
        0
    } else {
        text.match_indices('\n')
            .nth(loc.line as usize - 2)
            .map_or(text.len(), |(i, _)| i + 1)
    };
    text[line_start..]
        .char_indices()
        .nth((loc.column as usize).saturating_sub(1))
        .map_or(text.len(), |(i, _)| line_start + i)
}

/// The 1-based line and character column of byte `offset` in `src`.
#[allow(dead_code)] // used by the stage-1 rules, next part
pub fn source_position(src: &str, offset: usize) -> (usize, usize) {
    let before = &src[..offset];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    (line, before[line_start..].chars().count() + 1)
}

/// Where `loc`, a position in `model.text`, is in `src`. The neutraliser
/// keeps byte offsets and line breaks, so the line is the same; the column
/// is counted again in the source, where a Jinja tag earlier on the line
/// may have held characters wider than its placeholder.
#[allow(dead_code)] // used by the stage-1 rules, next part
pub fn map_to_source(src: &str, model_text: &str, loc: Location) -> (usize, usize) {
    let at = offset(model_text, loc);
    let (line, column) = source_position(src, at);
    assert_eq!(
        line as u64, loc.line,
        "the neutraliser keeps every line break, so lines never move"
    );
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full(src: &str) -> SqlDialect {
        match read(src, SqlDialect::Generic).parse {
            Parse::Full { dialect, .. } => dialect,
            Parse::Partial { cause, .. } => panic!("not parsed: {cause}"),
        }
    }

    fn partial(src: &str, pinned: SqlDialect) -> (String, usize) {
        match read(src, pinned).parse {
            Parse::Full { .. } => panic!("parsed: {src}"),
            Parse::Partial { cause, tokens } => (cause, tokens.len()),
        }
    }

    #[test]
    fn a_model_parses_under_the_generic_dialect_first() {
        assert_eq!(
            full("select o.id from {{ ref('orders') }} as o"),
            SqlDialect::Generic
        );
        // A model that is only a config() call is empty, and that parses.
        assert_eq!(
            full("{{ config(materialized='ephemeral') }}\n"),
            SqlDialect::Generic
        );
    }

    /// The recall pass's finding: Snowflake-only statements make the generic
    /// dialect reject the whole file. The retry parses it.
    #[test]
    fn a_snowflake_only_use_warehouse_parses_through_the_dialect_retry() {
        let src = "use warehouse ANALYTICS_PROD_WH;\nselect 1 from {{ ref('a') }}";
        assert!(Parser::parse_sql(&GenericDialect {}, "use warehouse X").is_err());
        assert_eq!(full(src), SqlDialect::Snowflake);
    }

    #[test]
    fn a_pinned_dialect_is_tried_alone() {
        let src = "use warehouse X;";
        let (cause, _) = partial(src, SqlDialect::Postgres);
        assert!(
            cause.starts_with("the postgres dialect (sql.dialect) doesn't parse it: line 1"),
            "{cause}"
        );
        assert!(matches!(
            read(src, SqlDialect::Snowflake).parse,
            Parse::Full {
                dialect: SqlDialect::Snowflake,
                ..
            }
        ));
    }

    /// A gap in every dialect, from the spec: named arguments inside
    /// `stream read_files(…)`.
    #[test]
    fn stream_read_files_with_named_arguments_parses_under_no_dialect() {
        let src = "select * from stream read_files('/Volumes/a/b', format => 'json')";
        let (cause, tokens) = partial(src, SqlDialect::Generic);
        assert!(cause.starts_with("no dialect parses it ("), "{cause}");
        // Every dialect stops at the same place, so the first tried is named.
        assert!(
            cause.starts_with("no dialect parses it (generic: "),
            "{cause}"
        );
        assert!(cause.contains("line 1, column"), "{cause}");
        assert!(cause.ends_with("checked from its tokens, so some findings may be missed"));
        assert!(tokens > 0);
    }

    /// The parser's error names a placeholder for what it stands for: the
    /// spec's loop that renders its own comma.
    #[test]
    fn a_placeholder_in_an_error_is_named_as_jinja() {
        let src = "select\n  a as x {{- \",\" if not loop.last }}\n  b\nfrom t";
        let (cause, _) = partial(src, SqlDialect::Generic);
        assert!(cause.contains("line 2, column 10: "), "{cause}");
        assert!(cause.contains("found a Jinja expression"), "{cause}");
    }

    #[test]
    fn jinja_that_cant_be_neutralised_is_read_from_tokens() {
        let (cause, tokens) = partial("select 1\nfrom {{ ref('x'\n", SqlDialect::Generic);
        assert_eq!(
            cause,
            "its Jinja can't be neutralised: unterminated Jinja tag on line 2; \
             checked from its tokens, so some findings may be missed"
        );
        assert!(tokens > 0, "the text before the tag is still read");
        let (cause, _) = partial("{% raw %}{{ x }}{% endraw %}", SqlDialect::Generic);
        assert!(cause.starts_with("its Jinja can't be neutralised: {% raw %} block on line 1"));
    }

    /// Without `recursive-protection`, the parser's own depth limit keeps
    /// deeply nested input from overflowing the stack: it is an error, and
    /// so `sql_not_parsed`, never a crash.
    #[test]
    fn deeply_nested_sql_is_a_parse_error_not_a_crash() {
        let depth = 5_000;
        let src = format!("select {}1{}", "(".repeat(depth), ")".repeat(depth));
        let (cause, tokens) = partial(&src, SqlDialect::Generic);
        assert!(cause.contains("recursion limit exceeded"), "{cause}");
        assert_eq!(tokens, 2 * depth + 3);
    }

    #[test]
    fn a_tokenizer_failure_says_nothing_was_checked() {
        let (cause, tokens) = partial("select 'unterminated", SqlDialect::Generic);
        assert_eq!(tokens, 0);
        assert!(cause.contains("tokenizer failed too"), "{cause}");
        assert!(cause.ends_with("so nothing in it was checked"), "{cause}");
    }

    #[test]
    fn positions_map_back_to_the_source() {
        // A tag holding wider characters than its placeholder: same line,
        // the column counted in the source.
        let src = "select {{ 'é—' if x }} as a, b\nfrom t";
        let model = read(src, SqlDialect::Generic);
        let Parse::Full { .. } = model.parse else {
            panic!("parses")
        };
        let tokens = Tokenizer::new(&GenericDialect {}, &model.text)
            .tokenize_with_location()
            .unwrap();
        let b = tokens.iter().find(|t| t.token.to_string() == "b").unwrap();
        assert_eq!(b.span.start, Location::new(1, 33));
        assert_eq!(map_to_source(src, &model.text, b.span.start), (1, 30));
        assert_eq!(&src[offset(&model.text, b.span.start)..][..1], "b");
    }

    /// Every token of every committed corpus model maps back to the same
    /// line, at the same byte, in the source.
    #[test]
    fn every_corpus_token_maps_back_to_its_line() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sql_corpus");
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("manifest.json")).unwrap())
                .unwrap();
        for m in manifest["models"].as_array().unwrap() {
            let src = std::fs::read_to_string(root.join(m["path"].as_str().unwrap())).unwrap();
            let model = read(&src, SqlDialect::Generic);
            let tokens = Tokenizer::new(&GenericDialect {}, &model.text)
                .tokenize_with_location()
                .unwrap();
            for t in tokens {
                let (line, column) = map_to_source(&src, &model.text, t.span.start);
                assert_eq!(line as u64, t.span.start.line);
                // The corpus has no wide characters inside a tag, so columns
                // agree too.
                assert_eq!(column as u64, t.span.start.column, "{}", m["path"]);
            }
        }
    }
}
