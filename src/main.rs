//! Spike for docs/design/sql-models.md: how well do SQL parsers read real dbt
//! models once their Jinja is neutralised, and how many seeded positives would
//! the proposed KV003 rules catch? Throwaway code on `spike/sql-parsers`,
//! never merged into kv-cli.
//!
//!   ./fetch.sh                                     # corpus + BigQuery grammar
//!   cargo run --release -- parse corpus synthetic  > results/parse.json
//!   cargo run --release -- recall corpus recall/seeds.json synthetic > results/recall.json
//!   cargo run --release -- probe < probes.sql > results/probes.txt
//!   python3 summarise.py results/parse.json results/recall.json
//!
//! The corpus is other projects' code, fetched at test time and never
//! committed. So the JSON written to stdout (committed under results/) holds
//! no text from a model: parser errors are reduced to their position, the
//! parser's own expectation and the class of the offending token, and hits are
//! reported without their subject unless the seed put it there. The full
//! error text goes to stderr for local reading.

use regex::Regex;
use serde_json::{json, Value};
use sqlparser::ast::{visit_relations, ObjectName, ObjectNamePart, Spanned, Statement, Use};
use sqlparser::dialect::{
    BigQueryDialect, DatabricksDialect, Dialect, GenericDialect, PostgreSqlDialect,
    SnowflakeDialect,
};
use sqlparser::parser::Parser as SqlParser;
use sqlparser::keywords::Keyword;
use sqlparser::tokenizer::{Token, TokenWithSpan, Tokenizer};
use std::ops::ControlFlow;
use std::path::Path;
use tree_sitter::{Language, Node, Parser as TsParser};

// --- Jinja neutralisation ----------------------------------------------------

/// Naive option (a): each `{{ … }}` becomes a same-length identifier
/// (`kv____`), except `{{ config(…) }}`, which renders nothing; each `{% … %}`
/// and `{# … #}` becomes spaces. Newlines are kept, so line numbers are exact.
fn neutralize_v1(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    while i + 1 < b.len() {
        let close: &[u8] = match &b[i..i + 2] {
            b"{{" => b"}}",
            b"{%" => b"%}",
            b"{#" => b"#}",
            _ => {
                i += 1;
                continue;
            }
        };
        let Some(rel) = b[i + 2..].windows(2).position(|w| w == close) else {
            break;
        };
        let end = i + 2 + rel + 2;
        let inner = src[i + 2..end - 2].trim_matches(|c: char| c == '-' || c == '+').trim();
        if close == b"}}" && !inner.starts_with("config") {
            placeholder(b, &mut out, i, end);
        } else {
            blank(&mut out, i, end);
        }
        i = end;
    }
    String::from_utf8(out).expect("ASCII replacements keep UTF-8")
}

fn blank(out: &mut [u8], s: usize, e: usize) {
    for k in s..e {
        if out[k] != b'\n' {
            out[k] = b' ';
        }
    }
}

/// `kv____` on the tag's first line, spaces after any newline.
fn placeholder(b: &[u8], out: &mut [u8], s: usize, e: usize) {
    let mut first = true;
    for k in s..e {
        if b[k] == b'\n' {
            first = false;
            continue;
        }
        out[k] = if !first {
            b' '
        } else if k == s {
            b'k'
        } else if k == s + 1 {
            b'v'
        } else {
            b'_'
        };
    }
}

/// The refined option (a), as the design document describes it:
/// - tag boundaries are found outside quoted strings, so
///   `{{ config(post_hook="{{ this }}") }}` is one tag;
/// - blocks that render nothing are blanked whole: `{% set x %}…{% endset %}`,
///   `{% macro %}`, `{% docs %}`;
/// - of `{% if %}…{% elif %}…{% else %}…{% endif %}` only the first branch is
///   kept, and a `{% for %}` body once.
///
/// `literal_passthrough` is the refinement the recall pass tests: a `{{ … }}`
/// that is a single quoted string renders as that string, padded with spaces.
fn neutralize_v2(src: &str, literal_passthrough: bool) -> String {
    let b = src.as_bytes();
    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        Expr,
        Stmt,
        Comment,
    }
    let mut tags: Vec<(usize, usize, Kind, String)> = Vec::new();
    let mut i = 0;
    while i + 1 < b.len() {
        let (kind, close) = match &b[i..i + 2] {
            b"{{" => (Kind::Expr, b"}}"),
            b"{%" => (Kind::Stmt, b"%}"),
            b"{#" => (Kind::Comment, b"#}"),
            _ => {
                i += 1;
                continue;
            }
        };
        let mut j = i + 2;
        let mut quote: Option<u8> = None;
        let mut end = None;
        while j + 1 < b.len() {
            let c = b[j];
            match quote {
                Some(q) if c == q => quote = None,
                Some(_) => {}
                None if kind != Kind::Comment && (c == b'\'' || c == b'"') => quote = Some(c),
                None if &b[j..j + 2] == close => {
                    end = Some(j + 2);
                    break;
                }
                None => {}
            }
            j += 1;
        }
        let Some(end) = end else { break };
        let inner = src[i + 2..end - 2]
            .trim_matches(|c: char| c == '-' || c == '+')
            .trim()
            .to_string();
        tags.push((i, end, kind, inner));
        i = end;
    }
    enum Frame {
        If { blank_from: Option<usize> },
        NoOutput { start: usize },
        Other,
    }
    let mut stack: Vec<Frame> = Vec::new();
    let mut blanks: Vec<(usize, usize)> = Vec::new();
    let mut idents: Vec<(usize, usize)> = Vec::new();
    let mut literals: Vec<(usize, usize, String)> = Vec::new();
    let string_literal = Regex::new(r#"^(?:'([^'\\]*)'|"([^"\\]*)")$"#).unwrap();
    for (start, end, kind, inner) in &tags {
        match kind {
            Kind::Comment => blanks.push((*start, *end)),
            Kind::Expr => {
                if inner.starts_with("config") {
                    blanks.push((*start, *end));
                } else if let Some(c) = string_literal.captures(inner).filter(|_| literal_passthrough) {
                    let text = c.get(1).or(c.get(2)).unwrap().as_str().to_string();
                    literals.push((*start, *end, text));
                } else {
                    idents.push((*start, *end));
                }
            }
            Kind::Stmt => {
                blanks.push((*start, *end));
                match inner.split_whitespace().next().unwrap_or("") {
                    "if" => stack.push(Frame::If { blank_from: None }),
                    "elif" | "else" => {
                        if let Some(Frame::If { blank_from }) = stack.last_mut() {
                            if blank_from.is_none() {
                                *blank_from = Some(*start);
                            }
                        }
                    }
                    "endif" => {
                        if let Some(Frame::If { blank_from: Some(from) }) = stack.pop() {
                            blanks.push((from, *start));
                        }
                    }
                    "set" if !inner.contains('=') => stack.push(Frame::NoOutput { start: *start }),
                    "macro" | "docs" | "test" | "materialization" => {
                        stack.push(Frame::NoOutput { start: *start })
                    }
                    "endset" | "endmacro" | "enddocs" | "endtest" | "endmaterialization" => {
                        if let Some(Frame::NoOutput { start: s }) = stack.pop() {
                            blanks.push((s, *end));
                        }
                    }
                    "for" | "call" | "filter" => stack.push(Frame::Other),
                    "endfor" | "endcall" | "endfilter" => {
                        stack.pop();
                    }
                    _ => {}
                }
            }
        }
    }
    let mut out = b.to_vec();
    for (s, e) in &idents {
        placeholder(b, &mut out, *s, *e);
    }
    for (s, e, text) in &literals {
        blank(&mut out, *s, *e);
        if !src[*s..*e].contains('\n') && text.is_ascii() {
            out[*s..*s + text.len()].copy_from_slice(text.as_bytes());
        } else {
            placeholder(b, &mut out, *s, *e);
        }
    }
    for (s, e) in &blanks {
        blank(&mut out, *s, *e);
    }
    String::from_utf8(out).expect("ASCII replacements keep UTF-8")
}

// --- tree-sitter measurements ---------------------------------------------

struct TsStats {
    clean: bool,
    error_spans: Vec<(usize, usize)>,
    relations: usize,
    relations_in_error: usize,
}

fn ts_stats(lang: &Language, text: &str) -> TsStats {
    let mut p = TsParser::new();
    p.set_language(lang).expect("grammar loads");
    let tree = p.parse(text, None).expect("tree-sitter parses");
    let root = tree.root_node();
    let mut s = TsStats {
        clean: !root.has_error(),
        error_spans: Vec::new(),
        relations: 0,
        relations_in_error: 0,
    };
    fn walk(n: Node, in_error: bool, s: &mut TsStats) {
        let is_err = n.is_error() || n.is_missing();
        if n.is_error() && !in_error {
            s.error_spans.push((n.start_byte(), n.end_byte()));
        }
        if n.kind() == "relation" {
            if in_error || is_err {
                s.relations_in_error += 1;
            } else {
                s.relations += 1;
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            walk(child, in_error || is_err, s);
        }
    }
    walk(root, false, &mut s);
    s
}

fn nonws(text: &str) -> usize {
    text.bytes().filter(|c| !c.is_ascii_whitespace()).count()
}

fn native_dialect(name: &str) -> Box<dyn Dialect> {
    match name {
        "snowflake" => Box::new(SnowflakeDialect {}),
        "bigquery" => Box::new(BigQueryDialect {}),
        "databricks" => Box::new(DatabricksDialect {}),
        "postgres" => Box::new(PostgreSqlDialect {}),
        other => panic!("unknown dialect {other}"),
    }
}

fn bigquery_grammar() -> Language {
    // The crate's Rust binding needs tree-sitter < 0.23 and cannot be linked
    // beside 0.26; build.rs compiles its fetched parser.c (ABI 14) instead.
    unsafe extern "C" {
        fn tree_sitter_sql_bigquery() -> *const ();
    }
    unsafe { tree_sitter_language::LanguageFn::from_raw(tree_sitter_sql_bigquery) }.into()
}

fn manifest(corpus: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(Path::new(corpus).join("manifest.json")).unwrap();
    serde_json::from_str::<Value>(&text).unwrap().as_array().unwrap().clone()
}

fn read_model(corpus: &str, m: &Value) -> String {
    let f = Path::new(corpus).join(m["file"].as_str().unwrap());
    std::fs::read_to_string(&f).unwrap_or_else(|e| panic!("{}: {e} (run ./fetch.sh)", f.display()))
}

/// sqlparser-rs's error, without the source text it quotes: the parser's own
/// expectation, the class of the token it found, and the position.
fn redact_error(e: &str) -> Value {
    let re = Regex::new(r"^sql parser error: Expected: (.*), found: (.*) at Line: (\d+), Column: (\d+)$").unwrap();
    match re.captures(e) {
        Some(c) => {
            let found = c.get(2).unwrap().as_str();
            let class = if is_placeholder(found) {
                "jinja placeholder"
            } else if found == "EOF" {
                "end of input"
            } else if found.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
                "word"
            } else {
                "punctuation or operator"
            };
            json!({
                "expected": c.get(1).unwrap().as_str(),
                "found": class,
                "line": c.get(3).unwrap().as_str().parse::<u64>().unwrap(),
                "column": c.get(4).unwrap().as_str().parse::<u64>().unwrap(),
            })
        }
        None => json!({"unclassified": true}),
    }
}

/// The `from`/`join` targets of a neutralised model, classified from
/// sqlparser-rs's tokens (so comments and strings are ignored). Approximate:
/// a comma-joined second table isn't counted, and `extract(… from x)`,
/// `trim(… from x)`, `substring(… from …)` and `is distinct from` are skipped.
fn from_join_targets(text: &str, dialect: &dyn Dialect) -> Value {
    let tokens: Vec<Token> = match Tokenizer::new(dialect, text).tokenize() {
        Ok(t) => t.into_iter().filter(|t| !matches!(t, Token::Whitespace(_))).collect(),
        Err(_) => return Value::Null,
    };
    let word = |k: usize| match tokens.get(k) {
        Some(Token::Word(w)) => Some(w),
        _ => None,
    };
    // CTE names: `name as (` and `name (cols) as (`.
    let mut ctes = std::collections::HashSet::new();
    for k in 0..tokens.len() {
        if let Some(w) = word(k) {
            let mut j = k + 1;
            if tokens.get(j) == Some(&Token::LParen) {
                while j < tokens.len() && tokens[j] != Token::RParen {
                    j += 1;
                }
                j += 1;
            }
            if word(j).is_some_and(|a| a.keyword == Keyword::AS) && tokens.get(j + 1) == Some(&Token::LParen) {
                ctes.insert(w.value.to_lowercase());
            }
        }
    }
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for c in ["placeholder", "cte", "one_part", "two_part", "three_part", "function", "subquery"] {
        counts.insert(c, 0);
    }
    let mut parens: Vec<Option<String>> = Vec::new();
    for k in 0..tokens.len() {
        match &tokens[k] {
            Token::LParen => parens.push(word(k.wrapping_sub(1)).map(|w| w.value.to_lowercase())),
            Token::RParen => {
                parens.pop();
            }
            _ => {}
        }
        let Some(w) = word(k) else { continue };
        if !matches!(w.keyword, Keyword::FROM | Keyword::JOIN) {
            continue;
        }
        if w.keyword == Keyword::FROM {
            let inside = parens.last().cloned().flatten();
            if matches!(inside.as_deref(), Some("extract" | "trim" | "substring" | "position" | "overlay")) {
                continue;
            }
            if word(k.wrapping_sub(1)).is_some_and(|p| p.keyword == Keyword::DISTINCT) {
                continue;
            }
        }
        let mut j = k + 1;
        if word(j).is_some_and(|n| matches!(n.keyword, Keyword::LATERAL | Keyword::STREAM)) {
            j += 1;
        }
        let class = match tokens.get(j) {
            Some(Token::LParen) => "subquery",
            Some(Token::Word(first)) => {
                let mut parts = vec![first.value.clone()];
                let mut n = j + 1;
                while tokens.get(n) == Some(&Token::Period) && word(n + 1).is_some() {
                    parts.push(word(n + 1).unwrap().value.clone());
                    n += 2;
                }
                if parts.iter().any(|p| is_placeholder(p)) {
                    "placeholder"
                } else if tokens.get(n) == Some(&Token::LParen) {
                    "function"
                } else {
                    match parts.len() {
                        1 if ctes.contains(&parts[0].to_lowercase()) => "cte",
                        1 => "one_part",
                        2 => "two_part",
                        _ => "three_part",
                    }
                }
            }
            _ => continue,
        };
        *counts.get_mut(class).unwrap() += 1;
    }
    json!(counts)
}

fn dialects() -> [(&'static str, Box<dyn Dialect>); 4] {
    [
        ("snowflake", Box::new(SnowflakeDialect {})),
        ("bigquery", Box::new(BigQueryDialect {})),
        ("databricks", Box::new(DatabricksDialect {})),
        ("postgres", Box::new(PostgreSqlDialect {})),
    ]
}

/// Every root that has a manifest.json: the fetched corpus, and the
/// synthetic models committed beside the harness.
fn models(roots: &[String]) -> Vec<(String, Value)> {
    roots
        .iter()
        .flat_map(|r| manifest(r).into_iter().map(move |m| (r.clone(), m)))
        .collect()
}

fn is_synthetic(m: &Value) -> bool {
    m["repo"] == "synthetic"
}

/// `parse`: the measurements in the design document's decision 3.
fn cmd_parse(roots: &[String]) {
    let sequel: Language = tree_sitter_sequel::LANGUAGE.into();
    let bigquery = bigquery_grammar();
    let mut rows = Vec::new();
    for (root, m) in models(roots) {
        let raw = read_model(&root, &m);
        let v1 = neutralize_v1(&raw);
        let v2 = neutralize_v2(&raw, false);
        for t in [&v1, &v2] {
            assert_eq!(t.len(), raw.len(), "same byte length");
            assert_eq!(t.lines().count(), raw.lines().count(), "same line count");
        }
        let dialect = m["dialect"].as_str().unwrap();
        let native = native_dialect(dialect);
        let sp = |d: &dyn Dialect, t: &str| SqlParser::parse_sql(d, t).map(|_| ()).map_err(|e| e.to_string());
        let v2_err = sp(&*native, &v2).err();
        if let Some(e) = &v2_err {
            eprintln!("{}: {e}", m["file"].as_str().unwrap());
        }
        let any_dialect = sp(&GenericDialect {}, &v2).is_ok() || dialects().iter().any(|(_, d)| sp(&**d, &v2).is_ok());
        let ts1 = ts_stats(&sequel, &v1);
        let ts2 = ts_stats(&sequel, &v2);
        let bq2 = ts_stats(&bigquery, &v2);
        let err_share = ts2
            .error_spans
            .iter()
            .map(|(s, e)| nonws(&v2[*s..*e]))
            .sum::<usize>() as f64
            / nonws(&v2).max(1) as f64;
        rows.push(json!({
            "dialect": dialect,
            "file": m["file"],
            "synthetic": is_synthetic(&m),
            "jinja_blocks": raw.matches("{%").count(),
            "sqlparser_raw_ok": sp(&*native, &raw).is_ok(),
            "sqlparser_v1_ok": sp(&*native, &v1).is_ok(),
            "sqlparser_v2_ok": v2_err.is_none(),
            "sqlparser_v2_err": v2_err.as_deref().map(redact_error),
            "sqlparser_generic_v2_ok": sp(&GenericDialect {}, &v2).is_ok(),
            "sqlparser_any_dialect_v2_ok": any_dialect,
            "tokenizer_v2_ok": Tokenizer::new(&*native, &v2).tokenize_with_location().is_ok(),
            "from_join_targets": from_join_targets(&v2, &*native),
            "ts_sequel_raw_clean": ts_stats(&sequel, &raw).clean,
            "ts_sequel_v1_clean": ts1.clean,
            "ts_sequel_v2_clean": ts2.clean,
            "ts_sequel_v2_error_share": err_share,
            "ts_sequel_v2_relations": ts2.relations,
            "ts_sequel_v2_relations_in_error": ts2.relations_in_error,
            "ts_bigquery_v2_clean": bq2.clean,
        }));
    }
    println!("{}", serde_json::to_string_pretty(&rows).unwrap());
}

// --- the proposed stage-one KV003 rules ------------------------------------

#[derive(Debug, Clone)]
struct Hit {
    rule: String,
    subject: String,
    line: u64,
    via: &'static str,
}

/// Lower-cased, unquoted, dot-joined: the `subject` of a relation finding.
fn normalise(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| p.trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']').to_lowercase())
        .collect::<Vec<_>>()
        .join(".")
}

fn is_placeholder(part: &str) -> bool {
    part.len() >= 2 && part.starts_with("kv") && part[2..].chars().all(|c| c == '_')
}

/// `information_schema`, `pg_catalog`, `snowflake.account_usage` and
/// `snowflake.organization_usage` are the same in every environment.
fn allowed(parts: &[String]) -> bool {
    let p: Vec<String> = parts.iter().map(|s| s.to_lowercase()).collect();
    p.iter().any(|s| s == "information_schema")
        || p.first().is_some_and(|s| s == "pg_catalog")
        || (p.len() >= 2 && p[0] == "snowflake" && (p[1] == "account_usage" || p[1] == "organization_usage"))
}

fn three_part(parts: &[String]) -> bool {
    parts.len() == 3 && !parts.iter().any(|p| is_placeholder(p)) && !allowed(parts)
}

fn object_parts(name: &ObjectName) -> Vec<String> {
    name.0
        .iter()
        .map(|p| match p {
            ObjectNamePart::Identifier(id) => id.value.clone(),
            other => other.to_string(),
        })
        .collect()
}

/// Byte offset of a 1-based (line, column) location, columns counted in chars.
fn offset(line_starts: &[usize], text: &str, line: u64, column: u64) -> usize {
    let start = line_starts[(line as usize).saturating_sub(1).min(line_starts.len() - 1)];
    text[start..]
        .char_indices()
        .nth((column as usize).saturating_sub(1))
        .map_or(text.len(), |(i, _)| start + i)
}

/// KV003 on one neutralised model: the AST when it parses, otherwise the
/// tokens (the fallback, which marks the file `partial`). String literals are
/// always judged from tokens. `raw` is the unneutralised text at the same
/// offsets, to tell a templated string (`'s3://{{ var("b") }}'`) apart.
fn kv003(raw: &str, text: &str, dialect: &dyn Dialect) -> (Vec<Hit>, bool) {
    let object_store = Regex::new(r#"\b(?:s3a?|gs|abfss?|wasbs?|gcs)://[^\s"']+"#).unwrap();
    let jdbc = Regex::new(r#"\bjdbc:[a-z0-9]+://[^\s"']+"#).unwrap();
    let mut hits = Vec::new();
    let tokens: Vec<TokenWithSpan> = Tokenizer::new(dialect, text)
        .tokenize_with_location()
        .unwrap_or_default();
    let parsed = SqlParser::parse_sql(dialect, text);
    let ok = parsed.is_ok();
    if let Ok(statements) = &parsed {
        let _ = visit_relations(statements, |name: &ObjectName| {
            let parts = object_parts(name);
            if three_part(&parts) {
                hits.push(Hit {
                    rule: "three_part_name".into(),
                    subject: normalise(&parts),
                    line: name.span().start.line,
                    via: "ast",
                });
            }
            ControlFlow::<()>::Continue(())
        });
        for st in statements {
            if let Statement::Use(u) = st {
                let (kind, name) = match u {
                    Use::Warehouse(n) => ("warehouse", n),
                    Use::Database(n) => ("database", n),
                    Use::Schema(n) => ("schema", n),
                    Use::Role(n) => ("role", n),
                    Use::Catalog(n) => ("catalog", n),
                    Use::Object(n) => ("object", n),
                    _ => continue,
                };
                hits.push(Hit {
                    rule: "use_statement".into(),
                    subject: format!("{kind} {}", normalise(&object_parts(name))),
                    line: name.span().start.line,
                    via: "ast",
                });
            }
        }
    } else {
        // Token fallback: the same relation and USE rules over the token stream.
        let words: Vec<(usize, &TokenWithSpan)> = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| !matches!(t.token, Token::Whitespace(_)))
            .collect();
        let word = |k: usize| match words.get(k).map(|(_, t)| &t.token) {
            Some(Token::Word(w)) => Some(w.value.clone()),
            _ => None,
        };
        let is = |k: usize, tok: &Token| words.get(k).is_some_and(|(_, t)| &t.token == tok);
        for k in 0..words.len() {
            let Some(w) = word(k) else { continue };
            let lw = w.to_lowercase();
            if matches!(lw.as_str(), "from" | "join" | "into" | "update" | "table") {
                if let (Some(a), true, Some(b2), true, Some(c)) =
                    (word(k + 1), is(k + 2, &Token::Period), word(k + 3), is(k + 4, &Token::Period), word(k + 5))
                {
                    let parts = vec![a, b2, c];
                    let following_period = is(k + 6, &Token::Period);
                    if three_part(&parts) && !following_period {
                        hits.push(Hit {
                            rule: "three_part_name".into(),
                            subject: normalise(&parts),
                            line: words[k + 1].1.span.start.line,
                            via: "tokens",
                        });
                    }
                }
            }
            if lw == "use" {
                if let (Some(kind), Some(name)) = (word(k + 1), word(k + 2)) {
                    let kind = kind.to_lowercase();
                    if matches!(kind.as_str(), "warehouse" | "database" | "schema" | "role") {
                        hits.push(Hit {
                            rule: "use_statement".into(),
                            subject: format!("{kind} {}", normalise(&[name])),
                            line: words[k + 2].1.span.start.line,
                            via: "tokens",
                        });
                    }
                }
            }
        }
    }
    // Object-store and JDBC paths in string literals, on either path.
    let mut line_starts = vec![0];
    line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    for (k, t) in tokens.iter().enumerate() {
        let s = match &t.token {
            Token::SingleQuotedString(s) | Token::DoubleQuotedString(s) => s,
            _ => continue,
        };
        let start = offset(&line_starts, text, t.span.start.line, t.span.start.column);
        let end = tokens
            .get(k + 1)
            .map_or(text.len(), |n| offset(&line_starts, text, n.span.start.line, n.span.start.column));
        let src = &raw[start..end.max(start)];
        if src.contains("{{") || src.contains("{%") {
            continue; // templated: varies by target
        }
        for (name, re) in [("object_store_uri", &object_store), ("jdbc_url", &jdbc)] {
            if let Some(m) = re.find(s) {
                hits.push(Hit {
                    rule: name.into(),
                    subject: format!("{name} {}", m.as_str()),
                    line: t.span.start.line,
                    via: "tokens",
                });
                break;
            }
        }
    }
    (hits, ok)
}

/// Apply one seed to a model's raw text. Returns the seeded text and the
/// expected (rule, subject, line).
fn seed(raw: &str, kind: &str) -> (String, String, String, u64) {
    let relation = Regex::new(r"(?i)\b(?:from|join)\s+(\{\{\s*(?:ref|source)\([^}]*\)\s*\}\})").unwrap();
    let first_line = |stmt: &str| format!("{stmt}\n{raw}");
    match kind {
        "three_part" | "jinja_three_part" => {
            let m = relation.captures(raw).expect("a from/join ref() or source() to replace");
            let tag = m.get(1).unwrap();
            let text = if kind == "three_part" {
                "ANALYTICS_PROD.RAW.ORDERS".to_string()
            } else {
                "{{ 'ANALYTICS_PROD.RAW.ORDERS' }}".to_string()
            };
            let line = raw[..tag.start()].matches('\n').count() as u64 + 1;
            let seeded = format!("{}{}{}", &raw[..tag.start()], text, &raw[tag.end()..]);
            (seeded, "three_part_name".into(), "analytics_prod.raw.orders".into(), line)
        }
        "use_warehouse" => (
            first_line("use warehouse ANALYTICS_PROD_WH;"),
            "use_statement".into(),
            "warehouse analytics_prod_wh".into(),
            1,
        ),
        "copy_into" => (
            first_line("copy into landing_orders from 's3://acme-prod-landing/orders/' file_format = (type = csv);"),
            "object_store_uri".into(),
            "object_store_uri s3://acme-prod-landing/orders/".into(),
            1,
        ),
        "location" => (
            first_line("create table if not exists landing_orders (id int) location 's3://acme-prod-lake/orders/';"),
            "object_store_uri".into(),
            "object_store_uri s3://acme-prod-lake/orders/".into(),
            1,
        ),
        other => panic!("unknown seed {other}"),
    }
}

/// A hit as committed: the subject only when it is one the seed wrote, so no
/// name from a third-party model reaches results/.
fn reported(h: &Hit, synthetic: bool, seeded: &str) -> String {
    let subject = if synthetic || h.subject == seeded { h.subject.as_str() } else { "<from the model>" };
    format!("{} {} line {} ({})", h.rule, subject, h.line, h.via)
}

/// `recall`: seed known positives into copies of models and check which the
/// rules catch; then run the rules over the unseeded models, where every hit
/// would be a false positive.
fn cmd_recall(corpus: &str, seeds: &str, synthetic_root: Option<&str>) {
    let seeds: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(seeds).unwrap()).unwrap();
    let models = manifest(corpus);
    let mut rows = Vec::new();
    for s in &seeds {
        let file = s["file"].as_str().unwrap();
        let m = models.iter().find(|m| m["file"] == file).expect("seed names a corpus model");
        let raw = read_model(corpus, m);
        let kind = s["seed"].as_str().unwrap();
        let (seeded, rule, subject, line) = seed(&raw, kind);
        let mut row = json!({"file": file, "seed": kind, "expect": {"rule": rule, "subject": subject, "line": line}});
        let native = native_dialect(m["dialect"].as_str().unwrap());
        let variants: [(&str, bool, &dyn Dialect); 3] = [
            ("as_proposed", false, &GenericDialect {}),
            ("with_literal_passthrough", true, &GenericDialect {}),
            ("native_dialect", false, &*native),
        ];
        for (label, passthrough, dialect) in variants {
            let text = neutralize_v2(&seeded, passthrough);
            assert_eq!(text.len(), seeded.len());
            let (hits, parsed) = kv003(&seeded, &text, dialect);
            let caught = hits.iter().find(|h| h.rule == rule && h.subject == subject && h.line == line);
            row[label] = json!({
                "parsed": parsed,
                "caught": caught.is_some(),
                "via": caught.map(|h| h.via),
                "other_hits": hits.iter().filter(|h| !(h.rule == rule && h.subject == subject && h.line == line))
                    .map(|h| reported(h, false, &subject)).collect::<Vec<_>>(),
            });
        }
        rows.push(row);
    }
    let unseeded = |root: &str, list: &[Value]| {
        let mut hits = Vec::new();
        let mut unparsed = 0;
        for m in list {
            let raw = read_model(root, m);
            let text = neutralize_v2(&raw, false);
            let (found, parsed) = kv003(&raw, &text, &GenericDialect {});
            if !parsed {
                unparsed += 1;
            }
            for h in found {
                eprintln!("unseeded hit {}: {} {} line {}", m["file"].as_str().unwrap(), h.rule, h.subject, h.line);
                hits.push(format!("{}: {}", m["file"].as_str().unwrap(), reported(&h, is_synthetic(m), "")));
            }
        }
        json!({"models": list.len(), "fell_back_to_tokens": unparsed, "hits": hits})
    };
    let synthetic = synthetic_root.map(|r| unseeded(r, &manifest(r)));
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "seeds": rows,
            "unseeded": unseeded(corpus, &models),
            "unseeded_synthetic": synthetic,
        }))
        .unwrap()
    );
}

/// `probe`: parse each line of stdin as one statement under every sqlparser-rs
/// dialect and with tree-sitter-sequel, for checking a single construct.
fn cmd_probe() {
    use std::io::BufRead;
    let sequel: Language = tree_sitter_sequel::LANGUAGE.into();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        if line.trim().is_empty() {
            continue;
        }
        let mut ok = vec![];
        if SqlParser::parse_sql(&GenericDialect {}, &line).is_ok() {
            ok.push("generic");
        }
        for (name, d) in dialects() {
            if SqlParser::parse_sql(&*d, &line).is_ok() {
                ok.push(name);
            }
        }
        let sp = if ok.is_empty() { "fails in every dialect".to_string() } else { ok.join(",") };
        let ts = if ts_stats(&sequel, &line).clean { "ts-sequel ok" } else { "ts-sequel fails" };
        println!("{sp:<42} {ts:<16} {line}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("parse") => cmd_parse(&args[2..]),
        Some("probe") => cmd_probe(),
        Some("recall") => cmd_recall(&args[2], &args[3], args.get(4).map(String::as_str)),
        _ => eprintln!("usage: harness parse <root>... | harness recall <corpus> <seeds.json> [<synthetic>]"),
    }
}
