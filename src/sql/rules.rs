//! KV003 on dbt SQL models: the stage-1 rules of docs/design/sql-models.md,
//! decision 4, with the identity of decision 6.
//!
//! - `three_part_name`: a literal `database.schema.table` relation.
//! - `two_part_name`: a literal `schema.table` relation, only with
//!   `sql.two_part_names: true`, and not when the first part is a CTE or a
//!   table alias, which makes it a correlated reference (`from orders o,
//!   o.items`).
//! - `use_warehouse`, `use_database`: `USE WAREHOUSE x`, `USE DATABASE x`.
//! - `object_store_uri`: an object-store URI written as a string literal in
//!   a `COPY INTO … FROM`/`TO` or a `LOCATION` clause, matched by the
//!   contract's `object_store_uri` value pattern.
//!
//! Relations and `USE` statements come from the syntax tree when the model
//! parsed, and from token-level versions of the same rules when it didn't.
//! String literals always come from tokens.
//!
//! Never reported: `ref()`/`source()` placeholders and any name or string
//! with a Jinja tag inside it (it varies by target); table functions;
//! anything `sql.allow_names` or `infrastructure.allow_values` allows; a line
//! carrying `infrastructure.ignore_marker` in a comment.

use super::neutralise::is_placeholder;
use super::parse::{self, Model, Parse};
use crate::audit::{finding_fingerprint, Engine, Finding, FindingKind, CODE_HARDCODED_INFRA};
use crate::python::wildcard_match;
use regex::Regex;
use sqlparser::ast::{
    Ident, ObjectName, ObjectNamePart, Spanned, Statement, TableAlias, TableFactor, Use, Visit,
    Visitor,
};
use sqlparser::keywords::Keyword;
use sqlparser::tokenizer::{Location, Token, TokenWithSpan, Tokenizer, Whitespace, Word};
use std::collections::{BTreeMap, HashSet};
use std::ops::ControlFlow;

/// One pinned thing, before allow lists and identity are applied.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hit {
    detector: &'static str,
    /// The name as written: relations and `USE`. `None` for a URI.
    key: Option<String>,
    subject: String,
    /// Where it starts in the model's text: the same byte in the source.
    at: Location,
    /// Allowed by `sql.allow_names`, which covers relations only.
    relation: bool,
}

/// A dotted name's identifiers, each with the quote it was written in.
type Parts = Vec<(String, Option<char>)>;

/// What the rules need from the contract.
struct Settings<'a> {
    two_part_names: bool,
    uri: Option<&'a Regex>,
}

/// KV003 findings for one model. `path` is the normalised path.
pub fn findings(engine: &Engine, path: &str, src: &str, model: &Model) -> Vec<Finding> {
    let infra = &engine.contract.infrastructure;
    if !infra.enabled {
        return Vec::new();
    }
    let sql = &engine.contract.sql;
    let settings = Settings {
        two_part_names: sql.two_part_names,
        uri: engine.infra_value_pattern("object_store_uri"),
    };
    let (tokens, partial) = match &model.parse {
        Parse::Full { dialect, .. } => (
            Tokenizer::new(parse::dialect(*dialect).as_ref(), &model.text)
                .tokenize_with_location()
                .unwrap_or_default(),
            false,
        ),
        Parse::Partial { tokens, .. } => (tokens.clone(), true),
    };
    let mut hits = match &model.parse {
        Parse::Full { statements, .. } => tree_hits(statements, &settings),
        Parse::Partial { .. } => token_hits(&tokens, &settings),
    };
    hits.extend(uri_hits(&tokens, &settings));

    let silenced = marked_lines(&tokens, &infra.ignore_marker);
    let allow_names: Vec<String> = sql.allow_names.iter().map(|p| p.to_lowercase()).collect();
    let mut kept: Vec<(Hit, usize)> = Vec::new();
    for hit in hits {
        let start = parse::offset(&model.text, hit.at);
        let (line, _) = parse::map_to_source(src, &model.text, hit.at);
        if templated(src, &model.text, start, hit_len(&hit, &model.text, start))
            || silenced.contains(&(line as u64))
            || infra
                .allow_values
                .iter()
                .any(|a| a.eq_ignore_ascii_case(&hit.subject))
            || (hit.relation && allow_names.iter().any(|p| wildcard_match(p, &hit.subject)))
        {
            continue;
        }
        kept.push((hit, line));
    }

    // One finding per pinned thing: the first occurrence carries it, and the
    // detail lists the others.
    kept.sort_by_key(|(h, line)| (*line, h.at.column));
    let mut by_subject: BTreeMap<String, (Hit, usize, Vec<usize>)> = BTreeMap::new();
    let mut order = Vec::new();
    for (hit, line) in kept {
        match by_subject.get_mut(&hit.subject) {
            Some((_, first, others)) => {
                if line != *first && !others.contains(&line) {
                    others.push(line);
                }
            }
            None => {
                order.push(hit.subject.clone());
                by_subject.insert(hit.subject.clone(), (hit, line, Vec::new()));
            }
        }
    }

    let symbol = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .strip_suffix(".sql")
        .or_else(|| path.rsplit('/').next())
        .unwrap_or(path)
        .to_string();
    order
        .into_iter()
        .map(|subject| {
            let (hit, line, others) = by_subject.remove(&subject).expect("collected");
            finding(path, &symbol, hit, line, &others, partial, infra.severity)
        })
        .collect()
}

fn finding(
    path: &str,
    symbol: &str,
    hit: Hit,
    line: usize,
    others: &[usize],
    partial: bool,
    severity: crate::config::Severity,
) -> Finding {
    let shown = hit.key.as_deref().unwrap_or(&hit.subject);
    let message = match hit.detector {
        "three_part_name" => format!("`{shown}` is a hardcoded three-part name"),
        "two_part_name" => format!("`{shown}` names its schema literally"),
        "use_warehouse" => format!("`USE WAREHOUSE {shown}` pins the warehouse"),
        "use_database" => format!("`USE DATABASE {shown}` pins the database"),
        _ => format!("`{shown}` is a hardcoded object-store path"),
    };
    let mut detail = advice(hit.detector).to_string();
    if !others.is_empty() {
        let lines: Vec<String> = others.iter().map(|l| l.to_string()).collect();
        detail.push_str(&format!(
            "; also on line{} {}",
            if others.len() == 1 { "" } else { "s" },
            lines.join(", ")
        ));
    }
    if partial {
        detail.push_str(" (read from the model's tokens: it doesn't parse)");
    }
    Finding {
        fingerprint: finding_fingerprint(path, CODE_HARDCODED_INFRA, symbol, &hit.subject),
        path: path.to_string(),
        line,
        code: CODE_HARDCODED_INFRA,
        severity,
        symbol: symbol.to_string(),
        subject: hit.subject,
        message,
        detail: Some(detail),
        framework: None,
        report_only: None,
        kind: FindingKind::HardcodedInfrastructure {
            detector: hit.detector.to_string(),
            key: hit.key,
        },
    }
}

/// What to do instead, per rule. `fix` lists the same advice.
pub fn advice(detector: &str) -> &'static str {
    match detector {
        "three_part_name" | "two_part_name" => {
            "use {{ ref() }} or {{ source() }}, so the name follows the target"
        }
        "use_warehouse" => "set the warehouse on the dbt target in profiles.yml",
        "use_database" => "set the database on the dbt target in profiles.yml",
        _ => "pass it in with var() or env_var(), so it follows the target",
    }
}

/// What `fix` tells a person to do, per rule: never done automatically.
pub fn fix_advice(detector: &str) -> &'static str {
    match detector {
        "three_part_name" | "two_part_name" => "replace it with {{ ref() }} or {{ source() }}",
        "use_warehouse" | "use_database" => "move it to the dbt target in profiles.yml",
        _ => "pass it in with var() or env_var()",
    }
}

/// The text a hit covers in the model, to check it for Jinja.
fn hit_len(hit: &Hit, text: &str, start: usize) -> usize {
    match &hit.key {
        // A name: as far as its last identifier character or quote.
        Some(_) => text[start..]
            .find(|c: char| {
                !(c.is_alphanumeric() || matches!(c, '_' | '.' | '"' | '`' | '[' | ']' | '-' | '$'))
            })
            .map_or(text.len(), |n| start + n),
        // A string literal: to its closing quote.
        None => {
            let quote = text[start..].chars().next().unwrap_or('\'');
            text[start + 1..]
                .find(quote)
                .map_or(text.len(), |n| start + 1 + n + 1)
        }
    }
}

/// True when the source text over `start..end` holds Jinja: a placeholder
/// the neutraliser wrote where the source has `{{`, or a statement or
/// comment tag. Such a name or string varies by target. A tag that passed
/// through as its literal string is not Jinja any more: it writes the name.
fn templated(src: &str, text: &str, start: usize, end: usize) -> bool {
    let (s, t) = (&src[start..end], &text[start..end]);
    s.contains("{%")
        || s.contains("{#")
        || t.match_indices("kv")
            .any(|(i, _)| src[start + i..].starts_with("{{"))
}

/// Lines carrying the ignore marker in a comment.
fn marked_lines(tokens: &[TokenWithSpan], marker: &str) -> HashSet<u64> {
    if marker.is_empty() {
        return HashSet::new();
    }
    tokens
        .iter()
        .filter(|t| match &t.token {
            Token::Whitespace(Whitespace::SingleLineComment { comment, .. }) => {
                comment.contains(marker)
            }
            Token::Whitespace(Whitespace::MultiLineComment(c)) => c.contains(marker),
            _ => false,
        })
        .map(|t| t.span.start.line)
        .collect()
}

/// Lower-cased, unquoted, dot-joined: a name's `subject`. A backtick-quoted
/// BigQuery identifier may hold the whole dotted path (`` `p.d.t` ``), so its
/// dots separate parts too.
fn parts_of(idents: &[(String, Option<char>)]) -> Vec<String> {
    idents
        .iter()
        .flat_map(|(value, quote)| {
            if *quote == Some('`') {
                value.split('.').map(str::to_string).collect::<Vec<_>>()
            } else {
                vec![value.clone()]
            }
        })
        .collect()
}

fn normalise(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| p.to_lowercase())
        .collect::<Vec<_>>()
        .join(".")
}

/// A relation's rule, if it has one: three or more parts, or two with the
/// opt-in rule on and a first part that isn't a CTE or alias in scope.
fn relation_rule(
    parts: &[String],
    scope: &HashSet<String>,
    settings: &Settings,
) -> Option<&'static str> {
    if parts.iter().any(|p| is_placeholder(p) || p.is_empty()) {
        return None;
    }
    match parts.len() {
        n if n >= 3 => Some("three_part_name"),
        2 if settings.two_part_names && !scope.contains(&parts[0].to_lowercase()) => {
            Some("two_part_name")
        }
        _ => None,
    }
}

// --- from the syntax tree ----------------------------------------------------

#[derive(Default)]
struct Collect {
    /// Every relation name, with its span start.
    relations: Vec<(ObjectName, Location)>,
    /// Span starts of names that are table functions: `from f(…)`.
    functions: HashSet<(u64, u64)>,
    /// CTE names and table aliases, lower-cased: a two-part name starting
    /// with one is a correlated reference.
    scope: HashSet<String>,
    uses: Vec<(&'static str, ObjectName)>,
}

fn alias_of(tf: &TableFactor) -> Option<&TableAlias> {
    match tf {
        TableFactor::Table { alias, .. }
        | TableFactor::Derived { alias, .. }
        | TableFactor::TableFunction { alias, .. }
        | TableFactor::Function { alias, .. }
        | TableFactor::UNNEST { alias, .. }
        | TableFactor::JsonTable { alias, .. }
        | TableFactor::OpenJsonTable { alias, .. }
        | TableFactor::NestedJoin { alias, .. }
        | TableFactor::Pivot { alias, .. }
        | TableFactor::Unpivot { alias, .. }
        | TableFactor::MatchRecognize { alias, .. }
        | TableFactor::XmlTable { alias, .. }
        | TableFactor::SemanticView { alias, .. } => alias.as_ref(),
        _ => None,
    }
}

impl Visitor for Collect {
    type Break = ();

    fn pre_visit_relation(&mut self, name: &ObjectName) -> ControlFlow<()> {
        self.relations.push((name.clone(), name.span().start));
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, tf: &TableFactor) -> ControlFlow<()> {
        if let TableFactor::Table {
            name,
            args: Some(_),
            ..
        } = tf
        {
            let at = name.span().start;
            self.functions.insert((at.line, at.column));
        }
        if let TableFactor::UNNEST {
            with_offset_alias: Some(a),
            ..
        } = tf
        {
            self.scope.insert(a.value.to_lowercase());
        }
        if let Some(a) = alias_of(tf) {
            self.scope.insert(a.name.value.to_lowercase());
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_query(&mut self, query: &sqlparser::ast::Query) -> ControlFlow<()> {
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.scope.insert(cte.alias.name.value.to_lowercase());
            }
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_statement(&mut self, st: &Statement) -> ControlFlow<()> {
        match st {
            Statement::Use(Use::Warehouse(n)) => self.uses.push(("use_warehouse", n.clone())),
            Statement::Use(Use::Database(n)) => self.uses.push(("use_database", n.clone())),
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

fn idents(name: &ObjectName) -> Option<Parts> {
    name.0
        .iter()
        .map(|p| match p {
            ObjectNamePart::Identifier(Ident {
                value, quote_style, ..
            }) => Some((value.clone(), *quote_style)),
            ObjectNamePart::Function(_) => None,
        })
        .collect()
}

fn tree_hits(statements: &[Statement], settings: &Settings) -> Vec<Hit> {
    let mut c = Collect::default();
    for st in statements {
        let _ = st.visit(&mut c);
    }
    let mut hits = Vec::new();
    for (name, at) in &c.relations {
        if at.line == 0 || c.functions.contains(&(at.line, at.column)) {
            continue;
        }
        let Some(idents) = idents(name) else { continue };
        let parts = parts_of(&idents);
        if let Some(detector) = relation_rule(&parts, &c.scope, settings) {
            hits.push(Hit {
                detector,
                key: Some(name.to_string()),
                subject: normalise(&parts),
                at: *at,
                relation: true,
            });
        }
    }
    for (detector, name) in &c.uses {
        let Some(idents) = idents(name) else { continue };
        let at = name.span().start;
        if at.line == 0 {
            continue;
        }
        hits.push(Hit {
            detector,
            key: Some(name.to_string()),
            subject: normalise(&parts_of(&idents)),
            at,
            relation: false,
        });
    }
    hits
}

// --- from tokens ------------------------------------------------------------

/// The non-whitespace tokens, comments included in whitespace.
fn significant(tokens: &[TokenWithSpan]) -> Vec<&TokenWithSpan> {
    tokens
        .iter()
        .filter(|t| !matches!(t.token, Token::Whitespace(_)))
        .collect()
}

fn word<'a>(toks: &[&'a TokenWithSpan], k: usize) -> Option<&'a Word> {
    match toks.get(k).map(|t| &t.token) {
        Some(Token::Word(w)) => Some(w),
        _ => None,
    }
}

fn is(toks: &[&TokenWithSpan], k: usize, tok: &Token) -> bool {
    toks.get(k).is_some_and(|t| &t.token == tok)
}

/// A dotted name starting at `k`: its parts as written and the index after
/// it.
fn chain(toks: &[&TokenWithSpan], k: usize) -> Option<(Parts, usize)> {
    let mut parts = vec![word(toks, k).map(|w| (w.value.clone(), w.quote_style))?];
    let mut n = k + 1;
    while is(toks, n, &Token::Period) {
        let Some(w) = word(toks, n + 1) else { break };
        parts.push((w.value.clone(), w.quote_style));
        n += 2;
    }
    Some((parts, n))
}

fn as_written(parts: &[(String, Option<char>)]) -> String {
    parts
        .iter()
        .map(|(v, q)| match q {
            Some('[') => format!("[{v}]"),
            Some(q) => format!("{q}{v}{q}"),
            None => v.clone(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The token-level rules for a model that didn't parse: relations after
/// `FROM`, `JOIN`, `INTO` and `UPDATE`, and `USE WAREHOUSE`/`USE DATABASE`.
/// `FROM` inside `extract(…)`, `trim(…)`, `substring(…)`, `position(…)` and
/// `overlay(…)`, and in `IS DISTINCT FROM`, isn't a relation.
fn token_hits(tokens: &[TokenWithSpan], settings: &Settings) -> Vec<Hit> {
    let toks = significant(tokens);
    // CTE names (`name as (`, `name (cols) as (`) and aliases after a
    // relation, for the two-part exception.
    let mut scope = HashSet::new();
    for k in 0..toks.len() {
        if let Some(w) = word(&toks, k) {
            let mut j = k + 1;
            if is(&toks, j, &Token::LParen) {
                while j < toks.len() && !is(&toks, j, &Token::RParen) {
                    j += 1;
                }
                j += 1;
            }
            if word(&toks, j).is_some_and(|a| a.keyword == Keyword::AS)
                && is(&toks, j + 1, &Token::LParen)
            {
                scope.insert(w.value.to_lowercase());
            }
        }
    }
    let mut candidates = Vec::new();
    let mut parens: Vec<Option<Keyword>> = Vec::new();
    let mut prev_word: Option<Keyword> = None;
    for k in 0..toks.len() {
        match &toks[k].token {
            Token::LParen => parens.push(prev_word),
            Token::RParen => {
                parens.pop();
            }
            _ => {}
        }
        prev_word = word(&toks, k).map(|w| w.keyword);
        let Some(w) = word(&toks, k) else { continue };
        if w.keyword == Keyword::USE {
            let detector = match word(&toks, k + 1).map(|w| w.keyword) {
                Some(Keyword::WAREHOUSE) => "use_warehouse",
                Some(Keyword::DATABASE) => "use_database",
                _ => continue,
            };
            if let Some((parts, _)) = chain(&toks, k + 2) {
                candidates.push((detector, parts, toks[k + 2].span.start));
            }
            continue;
        }
        if !matches!(
            w.keyword,
            Keyword::FROM | Keyword::JOIN | Keyword::INTO | Keyword::UPDATE
        ) {
            continue;
        }
        if w.keyword == Keyword::FROM {
            let inside = parens.last().copied().flatten();
            if matches!(
                inside,
                Some(
                    Keyword::EXTRACT
                        | Keyword::TRIM
                        | Keyword::SUBSTRING
                        | Keyword::POSITION
                        | Keyword::OVERLAY
                )
            ) || k
                .checked_sub(1)
                .and_then(|p| word(&toks, p))
                .is_some_and(|p| p.keyword == Keyword::DISTINCT)
            {
                continue;
            }
        }
        let mut j = k + 1;
        if word(&toks, j).is_some_and(|n| matches!(n.keyword, Keyword::LATERAL)) {
            j += 1;
        }
        let Some((parts, after)) = chain(&toks, j) else {
            continue;
        };
        if is(&toks, after, &Token::LParen) {
            continue; // a table function
        }
        // An alias after the relation is in scope for later two-part names.
        let mut a = after;
        if word(&toks, a).is_some_and(|w| w.keyword == Keyword::AS) {
            a += 1;
        }
        if let Some(alias) = word(&toks, a).filter(|w| w.keyword == Keyword::NoKeyword) {
            scope.insert(alias.value.to_lowercase());
        }
        candidates.push(("relation", parts, toks[j].span.start));
    }
    candidates
        .into_iter()
        .filter_map(|(kind, idents, at)| {
            let parts = parts_of(&idents);
            let detector = if kind == "relation" {
                relation_rule(&parts, &scope, settings)?
            } else {
                kind
            };
            Some(Hit {
                detector,
                key: Some(as_written(&idents)),
                subject: normalise(&parts),
                at,
                relation: kind == "relation",
            })
        })
        .collect()
}

/// `object_store_uri`, from tokens whether or not the model parsed: a string
/// literal directly after `LOCATION` (or `LOCATION =`), or after `FROM`,
/// `TO` or `INTO` in a statement that starts with `COPY`.
fn uri_hits(tokens: &[TokenWithSpan], settings: &Settings) -> Vec<Hit> {
    let Some(re) = settings.uri else {
        return Vec::new();
    };
    let toks = significant(tokens);
    let mut hits = Vec::new();
    let mut in_copy = false;
    let mut statement_start = true;
    for k in 0..toks.len() {
        if is(&toks, k, &Token::SemiColon) {
            statement_start = true;
            in_copy = false;
            continue;
        }
        if statement_start {
            in_copy = word(&toks, k).is_some_and(|w| w.keyword == Keyword::COPY);
            statement_start = false;
        }
        let value = match &toks[k].token {
            Token::SingleQuotedString(s) | Token::DoubleQuotedString(s) => s,
            _ => continue,
        };
        let before = |back: usize| k.checked_sub(back).and_then(|p| word(&toks, p));
        let after_location = before(1).is_some_and(|w| w.keyword == Keyword::LOCATION)
            || (k >= 2
                && is(&toks, k - 1, &Token::Eq)
                && before(2).is_some_and(|w| w.keyword == Keyword::LOCATION));
        let after_copy_clause = in_copy
            && before(1)
                .is_some_and(|w| matches!(w.keyword, Keyword::FROM | Keyword::TO | Keyword::INTO));
        if !(after_location || after_copy_clause) {
            continue;
        }
        if let Some(m) = re.find(value) {
            hits.push(Hit {
                detector: "object_store_uri",
                key: None,
                subject: m.as_str().to_string(),
                at: toks[k].span.start,
                relation: false,
            });
        }
    }
    hits
}

/// A tokenizer the rules can use for text with no known dialect.
#[cfg(test)]
fn generic_tokens(text: &str) -> Vec<TokenWithSpan> {
    Tokenizer::new(&sqlparser::dialect::GenericDialect {}, text)
        .tokenize_with_location()
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Contract, SqlDialect};

    fn engine(yaml: &str) -> Engine {
        Engine::new(Contract::from_yaml(&format!("version: 1\n{yaml}")).unwrap()).unwrap()
    }

    /// `(detector, subject, line)` for each finding on `src` at
    /// `models/orders.sql`.
    fn found_with(yaml: &str, src: &str) -> Vec<(String, String, usize)> {
        let e = engine(yaml);
        let model = parse::read(src, SqlDialect::Generic);
        findings(&e, "models/orders.sql", src, &model)
            .into_iter()
            .map(|f| {
                let FindingKind::HardcodedInfrastructure { detector, .. } = f.kind else {
                    unreachable!()
                };
                (detector, f.subject, f.line)
            })
            .collect()
    }

    fn found(src: &str) -> Vec<(String, String, usize)> {
        found_with("", src)
    }

    fn one(detector: &str, subject: &str, line: usize) -> Vec<(String, String, usize)> {
        vec![(detector.to_string(), subject.to_string(), line)]
    }

    #[test]
    fn a_literal_three_part_relation_is_reported_on_its_line() {
        let src = "select o.order_id\nfrom ANALYTICS_PROD.RAW.ORDERS as o\n";
        assert_eq!(
            found(src),
            one("three_part_name", "analytics_prod.raw.orders", 2)
        );
    }

    /// The syntax tree reaches relations anywhere: subqueries in `WHERE`,
    /// `INSERT INTO`, `MERGE INTO`, `UPDATE` and `DELETE` targets.
    #[test]
    fn relations_are_found_wherever_the_tree_has_them() {
        let src = "select 1 from {{ ref('a') }} where id in (\n  select id from PROD.RAW.BANNED)";
        assert_eq!(found(src), one("three_part_name", "prod.raw.banned", 2));
        let insert = "insert into PROD.MART.ORDERS select * from {{ ref('o') }}";
        assert_eq!(found(insert), one("three_part_name", "prod.mart.orders", 1));
        let merge = "merge into PROD.MART.T as t using {{ ref('s') }} as s on t.id = s.id \
                     when matched then update set t.x = s.x";
        assert_eq!(found(merge), one("three_part_name", "prod.mart.t", 1));
        assert_eq!(
            found("update PROD.MART.T set x = 1"),
            one("three_part_name", "prod.mart.t", 1)
        );
        assert_eq!(
            found("delete from PROD.MART.T where x = 1"),
            one("three_part_name", "prod.mart.t", 1)
        );
    }

    #[test]
    fn quoting_and_case_are_normalised_away() {
        for name in [
            "ANALYTICS_PROD.RAW.ORDERS",
            "\"analytics_prod\".\"raw\".\"orders\"",
            "Analytics_Prod.Raw.Orders",
        ] {
            assert_eq!(
                found(&format!("select * from {name}")),
                one("three_part_name", "analytics_prod.raw.orders", 1),
                "{name}"
            );
        }
        // BigQuery: one backtick-quoted identifier holding the whole path.
        assert_eq!(
            found("select * from `acme-prod.raw.orders`"),
            one("three_part_name", "acme-prod.raw.orders", 1)
        );
    }

    #[test]
    fn placeholders_and_templated_names_are_never_reported() {
        assert_eq!(
            found("select * from {{ ref('orders') }} join {{ source('a', 'b') }} b on 1=1"),
            vec![]
        );
        // Part of the name comes from Jinja, so it varies by target.
        assert_eq!(
            found("select * from analytics_{{ target.name }}.raw.orders"),
            vec![]
        );
        assert_eq!(
            found("select * from {% if x %}a{% endif %}prod.raw.orders"),
            vec![]
        );
        // A tag that is one string writes the name: literal pass-through.
        assert_eq!(
            found("select * from {{ 'ANALYTICS_PROD.RAW.ORDERS' }}"),
            one("three_part_name", "analytics_prod.raw.orders", 1)
        );
    }

    #[test]
    fn legitimately_literal_names_are_allowed() {
        for src in [
            "select * from analytics.information_schema.tables",
            "select * from `region-us`.INFORMATION_SCHEMA.JOBS",
            "select * from snowflake.account_usage.query_history",
            "select * from snowflake.organization_usage.usage_in_currency_daily",
            "select * from pg_catalog.pg_tables",
            "select * from `bigquery-public-data`.samples.shakespeare",
            "select * from information_schema.columns",
        ] {
            assert_eq!(
                found_with("sql:\n  two_part_names: true\n", src),
                vec![],
                "{src}"
            );
        }
        // A table function is not a relation, whatever its name.
        assert_eq!(found("select * from analytics.utils.split_rows(1)"), vec![]);
        // The contract can extend the list, and allow_values applies too.
        let src = "select * from shared.ref.countries";
        assert_eq!(found(src).len(), 1);
        assert_eq!(
            found_with("sql:\n  allow_names: ['shared.*']\n", src),
            vec![]
        );
        assert_eq!(
            found_with(
                "infrastructure:\n  allow_values: ['SHARED.REF.COUNTRIES']\n",
                src
            ),
            vec![]
        );
    }

    #[test]
    fn the_ignore_marker_silences_a_line() {
        let src = "select *\nfrom PROD.RAW.ORDERS -- kovallent:allow-infra\njoin PROD.RAW.ITEMS using (id)";
        assert_eq!(found(src), one("three_part_name", "prod.raw.items", 3));
    }

    #[test]
    fn two_part_names_need_the_opt_in_and_skip_correlated_references() {
        let src = "select * from RAW.ORDERS";
        assert_eq!(found(src), vec![]);
        let on = "sql:\n  two_part_names: true\n";
        assert_eq!(found_with(on, src), one("two_part_name", "raw.orders", 1));
        // BigQuery's correlated `from t, t.items`, and a CTE's columns.
        let correlated = "with o as (select * from {{ ref('orders') }})\n\
                          select * from o, o.items";
        assert_eq!(found_with(on, correlated), vec![]);
        let aliased = "select * from {{ ref('orders') }} as ord, ord.items as i";
        assert_eq!(found_with(on, aliased), vec![]);
    }

    #[test]
    fn use_warehouse_and_use_database_are_reported() {
        assert_eq!(
            found("use warehouse ANALYTICS_PROD_WH;\nselect 1"),
            one("use_warehouse", "analytics_prod_wh", 1)
        );
        assert_eq!(
            found("use database ANALYTICS_PROD;\nselect 1"),
            one("use_database", "analytics_prod", 1)
        );
        // USE SCHEMA and USE ROLE are a later stage.
        assert_eq!(found("use schema RAW;\nuse role ADMIN;\nselect 1"), vec![]);
        assert_eq!(found("use warehouse {{ var('wh') }};\nselect 1"), vec![]);
    }

    #[test]
    fn object_store_paths_count_only_in_copy_into_and_location() {
        assert_eq!(
            found("copy into landing_orders from 's3://acme-prod-landing/orders/'"),
            one("object_store_uri", "s3://acme-prod-landing/orders/", 1)
        );
        assert_eq!(
            found("create table t (id int) location 's3://acme-prod-lake/orders/'"),
            one("object_store_uri", "s3://acme-prod-lake/orders/", 1)
        );
        // An unload: the path is COPY's target.
        assert_eq!(
            found("copy into 'gs://acme-exports/orders/' from {{ ref('orders') }}"),
            one("object_store_uri", "gs://acme-exports/orders/", 1)
        );
        // Anywhere else is a later stage.
        assert_eq!(
            found("select * from read_parquet('gs://acme/x.parquet')"),
            vec![]
        );
        assert_eq!(
            found("select 's3://acme/x' as path from {{ ref('a') }}"),
            vec![]
        );
        // A path with Jinja in it is templated.
        assert_eq!(
            found("copy into t from 's3://{{ var(\"bucket\") }}/orders/'"),
            vec![]
        );
        // The URI is the subject exactly as matched: case kept.
        assert_eq!(
            found("copy into t from 's3://Acme-Prod/Orders/'")[0].1,
            "s3://Acme-Prod/Orders/"
        );
    }

    /// One finding per pinned thing: the first line carries it and the
    /// detail lists the others.
    #[test]
    fn the_same_pinned_thing_twice_is_one_finding() {
        let e = engine("");
        let src = "select * from PROD.RAW.ORDERS\nunion all\nselect * from prod.raw.orders\n\
                   union all\nselect * from \"PROD\".\"RAW\".\"ORDERS\"";
        let model = parse::read(src, SqlDialect::Generic);
        let f = findings(&e, "models/orders.sql", src, &model);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].line, 1);
        assert!(
            f[0].detail
                .as_deref()
                .unwrap()
                .ends_with("; also on lines 3, 5"),
            "{:?}",
            f[0].detail
        );
    }

    #[test]
    fn identity_follows_the_owner_decisions() {
        let e = engine("");
        let src =
            "select * from ANALYTICS_PROD.RAW.ORDERS o\njoin ANALYTICS_PROD.RAW.CUSTOMERS c on 1=1";
        let model = parse::read(src, SqlDialect::Generic);
        let f = findings(&e, "models/orders.sql", src, &model);
        let got: Vec<(&str, &str, &str, Option<&str>)> = f
            .iter()
            .map(|f| {
                let FindingKind::HardcodedInfrastructure { key, .. } = &f.kind else {
                    unreachable!()
                };
                (
                    f.symbol.as_str(),
                    f.subject.as_str(),
                    f.fingerprint.as_str(),
                    key.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (
                    "orders",
                    "analytics_prod.raw.orders",
                    "803e1305b4fe391a",
                    Some("ANALYTICS_PROD.RAW.ORDERS")
                ),
                (
                    "orders",
                    "analytics_prod.raw.customers",
                    "424c475cee1fe19b",
                    Some("ANALYTICS_PROD.RAW.CUSTOMERS")
                ),
            ]
        );
        assert_eq!(
            f[0].message,
            "`ANALYTICS_PROD.RAW.ORDERS` is a hardcoded three-part name"
        );
        assert_eq!(
            f[0].detail.as_deref(),
            Some("use {{ ref() }} or {{ source() }}, so the name follows the target")
        );
        assert_eq!(f[0].severity, crate::config::Severity::Warning);
    }

    #[test]
    fn severity_and_enabled_come_from_infrastructure() {
        let src = "select * from PROD.RAW.ORDERS";
        let e = engine("infrastructure:\n  severity: error\n");
        let model = parse::read(src, SqlDialect::Generic);
        assert!(findings(&e, "m.sql", src, &model)[0].severity.is_error());
        assert_eq!(
            found_with("infrastructure:\n  enabled: false\n", src),
            vec![]
        );
    }

    /// The token fallback finds the same things, and its findings say they
    /// were read from tokens.
    #[test]
    fn a_model_that_does_not_parse_is_checked_from_its_tokens() {
        let src = "with {{ dedupe('o') }}\nselect extract(year from t.created_at), x\n\
                   from PROD.RAW.ORDERS t\njoin RAW.ITEMS i on i.id = t.id\n\
                   where a is distinct from b.c";
        let model = parse::read(src, SqlDialect::Generic);
        assert!(matches!(model.parse, Parse::Partial { .. }));
        assert_eq!(found(src), one("three_part_name", "prod.raw.orders", 3));
        let on = found_with("sql:\n  two_part_names: true\n", src);
        assert_eq!(
            on,
            vec![
                ("three_part_name".into(), "prod.raw.orders".into(), 3),
                ("two_part_name".into(), "raw.items".into(), 4)
            ]
        );
        let e = engine("");
        let f = findings(&e, "m.sql", src, &model);
        assert!(
            f[0].detail
                .as_deref()
                .unwrap()
                .ends_with("(read from the model's tokens: it doesn't parse)"),
            "{:?}",
            f[0].detail
        );
        // USE and COPY from tokens too.
        let src = "{{ broken(\nuse warehouse PROD_WH;\n";
        let model = parse::read(src, SqlDialect::Generic);
        assert!(matches!(model.parse, Parse::Partial { .. }));
        assert_eq!(found(src), vec![], "an unterminated tag blanks the rest");
        let src =
            "with {{ x() }} select 1;\nuse warehouse PROD_WH;\ncopy into t from 's3://acme/raw/'";
        assert_eq!(
            found(src),
            vec![
                ("use_warehouse".into(), "prod_wh".into(), 2),
                ("object_store_uri".into(), "s3://acme/raw/".into(), 3)
            ]
        );
    }

    #[test]
    fn token_helpers_read_dotted_names() {
        let toks = generic_tokens("from a.\"B\".`c` x");
        let sig = significant(&toks);
        let (parts, after) = chain(&sig, 1).unwrap();
        assert_eq!(as_written(&parts), "a.\"B\".`c`");
        assert_eq!(after, 6);
    }
}
