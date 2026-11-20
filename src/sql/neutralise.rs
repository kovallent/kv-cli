//! Jinja neutralisation: dbt model text in, plain SQL of the same shape out.
//!
//! kv-cli can't render Jinja: rendering needs `dbt compile`, the project's
//! packages and often a warehouse. So each tag is replaced by something a SQL
//! parser accepts, byte for byte in place, and every newline stays where it
//! was. A position in the output is the same byte offset, and the same line,
//! in the source. See docs/design/sql-models.md, decision 2.
//!
//! - `{{ … }}` becomes a same-length identifier, `kv____…`, on its first line
//!   and spaces after any newline. A placeholder parses as a column in
//!   expression position and as a table after `from`, so `ref()` and
//!   `source()` are never mistaken for literal names.
//! - `{{ config(…) }}` renders nothing, so it is blanked.
//! - `{{ 'text' }}`, a tag that is exactly one quoted string, renders as that
//!   string, padded with spaces to the tag's length (literal pass-through).
//! - `{% … %}` and `{# … #}` become spaces.
//! - Tag boundaries are found outside quoted strings, so
//!   `{{ config(post_hook="{{ this }}") }}` is one tag.
//! - Blocks that render nothing are blanked whole: `{% set x %}…{% endset %}`,
//!   `{% macro %}`, `{% docs %}`, `{% test %}`, `{% materialization %}`.
//! - Of `{% if %}…{% elif %}…{% else %}…{% endif %}` only the first branch is
//!   kept; a `{% for %}` body is kept once, and its `{% else %}` dropped.
//!
//! An unterminated tag or a `{% raw %}` block can't be neutralised. The
//! failure still carries a best-effort text of the same shape, for the
//! token-level fallback.

use regex::Regex;
use std::sync::OnceLock;

/// Why a model's Jinja can't be neutralised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// A `{{`, `{%` or `{#` with no closing delimiter.
    UnterminatedTag { line: usize },
    /// A `{% raw %}` block, whose body is literal text, braces included.
    RawBlock { line: usize },
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reason::UnterminatedTag { line } => {
                write!(f, "unterminated Jinja tag on line {line}")
            }
            Reason::RawBlock { line } => write!(f, "{{% raw %}} block on line {line}"),
        }
    }
}

/// A model whose Jinja can't be neutralised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub reason: Reason,
    /// Best effort, with the same length and newlines as the source: tags
    /// before the problem are neutralised as usual, an unterminated tag is
    /// blanked to the end of the file, and a `{% raw %}` body is kept as the
    /// literal text it renders.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Expr,
    Stmt,
    Comment,
}

struct Tag {
    start: usize,
    end: usize,
    kind: Kind,
    /// The tag's body, without delimiters, whitespace control or padding.
    inner: String,
}

/// Neutralise `src`. The result has `src`'s byte length, and a `\n` or `\r`
/// wherever `src` has one.
pub fn neutralise(src: &str) -> Result<String, Failure> {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let (tags, raws, problem, cut) = scan(src);

    let mut stack: Vec<Frame> = Vec::new();
    let mut blanks: Vec<(usize, usize)> = Vec::new();
    for tag in &tags {
        match tag.kind {
            Kind::Comment => blanks.push((tag.start, tag.end)),
            Kind::Expr => {
                if config_call().is_match(&tag.inner) {
                    blanks.push((tag.start, tag.end));
                } else if let Some(text) = single_string(&tag.inner)
                    .filter(|_| !src[tag.start..tag.end].contains(['\n', '\r']))
                {
                    blank(&mut out, tag.start, tag.end);
                    out[tag.start..tag.start + text.len()].copy_from_slice(text.as_bytes());
                } else {
                    placeholder(b, &mut out, tag.start, tag.end);
                }
            }
            Kind::Stmt => {
                blanks.push((tag.start, tag.end));
                statement(tag, &mut stack, &mut blanks);
            }
        }
    }
    // A block left open at the end renders as far as it goes.
    for frame in stack {
        match frame {
            Frame::Branching {
                drop_from: Some(from),
                ..
            } => blanks.push((from, b.len())),
            Frame::Silent { start, .. } => blanks.push((start, b.len())),
            _ => {}
        }
    }
    for (s, e) in &blanks {
        blank(&mut out, *s, *e);
    }
    for (s, e) in &raws {
        // The tags around a raw body render nothing; the body is literal.
        blank(&mut out, *s, *e);
    }

    let text = String::from_utf8(out).expect("ASCII written over whole UTF-8 sequences");
    match problem {
        None => Ok(text),
        Some(Reason::UnterminatedTag { line }) => {
            let mut bytes = text.into_bytes();
            blank(&mut bytes, cut.unwrap_or(b.len()), b.len());
            Err(Failure {
                reason: Reason::UnterminatedTag { line },
                text: String::from_utf8(bytes).expect("still UTF-8"),
            })
        }
        Some(reason) => Err(Failure { reason, text }),
    }
}

enum Frame {
    /// `if` or `for`: everything from `drop_from` (the first `elif` or
    /// `else`) to the closing tag is dropped.
    Branching {
        end: &'static str,
        drop_from: Option<usize>,
    },
    /// A block that renders nothing, blanked from `start` to its end tag.
    Silent { end: &'static str, start: usize },
    /// A block whose body renders once: `call`, `filter`, `snapshot`, …
    Plain { end: &'static str },
}

impl Frame {
    fn end(&self) -> &'static str {
        match self {
            Frame::Branching { end, .. } | Frame::Silent { end, .. } | Frame::Plain { end } => end,
        }
    }
}

fn statement(tag: &Tag, stack: &mut Vec<Frame>, blanks: &mut Vec<(usize, usize)>) {
    let word = tag.inner.split_whitespace().next().unwrap_or("");
    match word {
        "if" => stack.push(Frame::Branching {
            end: "endif",
            drop_from: None,
        }),
        "for" => stack.push(Frame::Branching {
            end: "endfor",
            drop_from: None,
        }),
        "elif" | "else" => {
            if let Some(Frame::Branching { drop_from, .. }) = stack.last_mut() {
                drop_from.get_or_insert(tag.start);
            }
        }
        // `{% set x = … %}` assigns inline; `{% set x %}…{% endset %}`
        // captures its body into a variable and renders nothing.
        "set" if !tag.inner.contains('=') => stack.push(Frame::Silent {
            end: "endset",
            start: tag.start,
        }),
        "macro" | "docs" | "test" | "materialization" => {
            let end = match word {
                "macro" => "endmacro",
                "docs" => "enddocs",
                "test" => "endtest",
                _ => "endmaterialization",
            };
            stack.push(Frame::Silent {
                end,
                start: tag.start,
            })
        }
        "call" | "filter" | "snapshot" | "block" | "with" => {
            let end = match word {
                "call" => "endcall",
                "filter" => "endfilter",
                "snapshot" => "endsnapshot",
                "block" => "endblock",
                _ => "endwith",
            };
            stack.push(Frame::Plain { end })
        }
        w if w.starts_with("end") => {
            // Close the nearest matching block; stray end tags are ignored.
            let Some(at) = stack.iter().rposition(|f| f.end() == w) else {
                return;
            };
            for frame in stack.drain(at..).rev() {
                match frame {
                    Frame::Branching {
                        drop_from: Some(from),
                        ..
                    } => blanks.push((from, tag.start)),
                    Frame::Silent { start, .. } => blanks.push((start, tag.end)),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Every tag, quote-aware; the spans of `{% raw %}` and `{% endraw %}`; the
/// first problem; and where an unterminated tag starts, since scanning stops
/// there.
type Scanned = (Vec<Tag>, Vec<(usize, usize)>, Option<Reason>, Option<usize>);

fn scan(src: &str) -> Scanned {
    let b = src.as_bytes();
    let mut tags = Vec::new();
    let mut raws = Vec::new();
    let mut problem = None;
    let mut cut = None;
    let line_of = |i: usize| src[..i].matches('\n').count() + 1;
    let mut i = 0;
    while i + 1 < b.len() {
        let (kind, close): (Kind, &[u8; 2]) = match &b[i..i + 2] {
            b"{{" => (Kind::Expr, b"}}"),
            b"{%" => (Kind::Stmt, b"%}"),
            b"{#" => (Kind::Comment, b"#}"),
            _ => {
                i += 1;
                continue;
            }
        };
        let Some(end) = close_of(b, i + 2, kind, close) else {
            problem.get_or_insert(Reason::UnterminatedTag { line: line_of(i) });
            cut = Some(i);
            break;
        };
        let inner = src[i + 2..end - 2]
            .trim()
            .trim_start_matches(['-', '+'])
            .trim_end_matches(['-', '+'])
            .trim()
            .to_string();
        if kind == Kind::Stmt && inner == "raw" {
            problem.get_or_insert(Reason::RawBlock { line: line_of(i) });
            raws.push((i, end));
            match endraw().find(&src[end..]) {
                Some(m) => {
                    raws.push((end + m.start(), end + m.end()));
                    i = end + m.end();
                }
                // Raw to the end of the file: all of it is literal text.
                None => i = b.len(),
            }
            continue;
        }
        tags.push(Tag {
            start: i,
            end,
            kind,
            inner,
        });
        i = end;
    }
    (tags, raws, problem, cut)
}

/// The offset just past `close`, searching from `from`. Inside `{{ }}` and
/// `{% %}`, delimiters within a quoted string don't count; comments have no
/// strings.
fn close_of(b: &[u8], from: usize, kind: Kind, close: &[u8; 2]) -> Option<usize> {
    let mut quote: Option<u8> = None;
    let mut j = from;
    while j + 1 < b.len() {
        let c = b[j];
        match quote {
            Some(_) if c == b'\\' => j += 1,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if kind != Kind::Comment && (c == b'\'' || c == b'"') => quote = Some(c),
            None if &b[j..j + 2] == close => return Some(j + 2),
            None => {}
        }
        j += 1;
    }
    None
}

/// `config(…)`, which renders nothing.
fn config_call() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^config\s*\(").unwrap())
}

fn endraw() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{%[-+]?\s*endraw\s*[-+]?%\}").unwrap())
}

/// The text of an expression that is exactly one quoted string, without
/// escapes: `'ANALYTICS_PROD.RAW.ORDERS'` gives `ANALYTICS_PROD.RAW.ORDERS`.
fn single_string(inner: &str) -> Option<&str> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r#"^(?:'([^'\\]*)'|"([^"\\]*)")$"#).unwrap());
    let c = re.captures(inner)?;
    Some(c.get(1).or(c.get(2))?.as_str())
}

fn is_line_break(c: u8) -> bool {
    c == b'\n' || c == b'\r'
}

fn blank(out: &mut [u8], s: usize, e: usize) {
    for c in &mut out[s..e] {
        if !is_line_break(*c) {
            *c = b' ';
        }
    }
}

/// `kv____` on the tag's first line, spaces after any newline.
fn placeholder(b: &[u8], out: &mut [u8], s: usize, e: usize) {
    let mut first = true;
    for k in s..e {
        if is_line_break(b[k]) {
            first &= b[k] != b'\n';
            out[k] = b[k];
            continue;
        }
        out[k] = match (first, k - s) {
            (false, _) => b' ',
            (true, 0) => b'k',
            (true, 1) => b'v',
            (true, _) => b'_',
        };
    }
}

/// True for a placeholder this module writes: `kv` and one or more `_`.
pub fn is_placeholder(word: &str) -> bool {
    word.len() > 2 && word.starts_with("kv") && word[2..].bytes().all(|c| c == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) -> String {
        let out = neutralise(src).unwrap_or_else(|f| panic!("{f:?}"));
        assert_same_shape(src, &out);
        out
    }

    /// The placeholder for a tag of `tag`'s length.
    fn ph(tag: &str) -> String {
        format!("kv{}", "_".repeat(tag.len() - 2))
    }

    /// Spaces the length of `tag`.
    fn sp(tag: &str) -> String {
        " ".repeat(tag.len())
    }

    /// Same length, and a line break wherever the source has one.
    fn assert_same_shape(src: &str, out: &str) {
        assert_eq!(out.len(), src.len(), "byte length\n{src:?}\n{out:?}");
        for (i, (a, b)) in src.bytes().zip(out.bytes()).enumerate() {
            assert_eq!(is_line_break(a), is_line_break(b), "byte {i}");
            if is_line_break(a) {
                assert_eq!(a, b, "byte {i}");
            }
        }
    }

    #[test]
    fn expressions_become_placeholders_and_config_is_blanked() {
        let config = "{{ config(materialized='table') }}";
        let r = "{{ ref('orders') }}";
        assert_eq!(
            ok(&format!("{config}\nselect * from {r}")),
            format!("{}\nselect * from {}", sp(config), ph(r))
        );
    }

    #[test]
    fn statements_and_comments_become_spaces() {
        let (s, c) = ("{% do x %}", "{# note #}");
        assert_eq!(ok(&format!("a{s}b{c}c")), format!("a{}b{}c", sp(s), sp(c)));
    }

    #[test]
    fn a_multi_line_expression_keeps_its_newlines() {
        let (first, second) = ("{{ ref(", "  'x') }}");
        assert_eq!(
            ok(&format!("from {first}\n{second} t")),
            format!("from {}\n{} t", ph(first), sp(second))
        );
    }

    /// Quote-aware boundaries: a `}}` inside a string doesn't close the tag.
    #[test]
    fn tag_boundaries_ignore_delimiters_inside_strings() {
        let hook = "{{ config(post_hook=\"{{ this }}\") }}";
        assert_eq!(
            ok(&format!("{hook}select 1")),
            format!("{}select 1", sp(hook))
        );
        let escaped = r"{{ x('it\'s }}') }}";
        assert_eq!(ok(&format!("{escaped} y")), format!("{} y", ph(escaped)));
    }

    #[test]
    fn blocks_that_render_nothing_are_blanked_whole() {
        let src = "a{% set q %}select 9{% endset %}b{% macro m() %}x{% endmacro %}c\
                   {% docs d %}text{% enddocs %}d{% test t() %}1{% endtest %}e";
        assert_eq!(ok(src).replace(' ', ""), "abcde");
        // An inline set renders nothing but opens no block.
        let set = "{% set x = 1 %}";
        assert_eq!(ok(&format!("{set}y")), format!("{}y", sp(set)));
    }

    #[test]
    fn only_the_first_branch_of_an_if_is_kept() {
        let src = "{% if a %}one{% elif b %}two{% else %}three{% endif %}!";
        assert_eq!(ok(src).replace(' ', ""), "one!");
        let nested = "{% if a %}{% if b %}x{% else %}y{% endif %}{% else %}z{% endif %}";
        assert_eq!(ok(nested).replace(' ', ""), "x");
    }

    #[test]
    fn a_for_body_is_kept_once_and_its_else_dropped() {
        let c = "{{ c }}";
        let src = format!("{{% for c in cols %}}{c},{{% else %}}none{{% endfor %}}x");
        assert_eq!(ok(&src).replace(' ', ""), format!("{},x", ph(c)));
    }

    #[test]
    fn a_snapshot_body_is_kept() {
        let src = "{% snapshot s %}select 1{% endsnapshot %}";
        assert_eq!(ok(src).trim(), "select 1");
    }

    /// Literal pass-through: the string a tag writes, padded to its length.
    #[test]
    fn a_tag_that_is_one_string_renders_as_that_string() {
        let tag = "{{ 'ANALYTICS_PROD.RAW.ORDERS' }}";
        let pad = " ".repeat(tag.len() - "ANALYTICS_PROD.RAW.ORDERS".len());
        assert_eq!(
            ok(&format!("select * from {tag} o")),
            format!("select * from ANALYTICS_PROD.RAW.ORDERS{pad} o")
        );
        let double = "{{ \"x\" }}";
        assert_eq!(ok(double), format!("x{}", " ".repeat(double.len() - 1)));
        // Anything more than one string is an expression.
        for expr in ["{{ 'a' ~ 'b' }}", "{{ 'a' if b }}", "{{ 'a' | upper }}"] {
            assert_eq!(ok(expr), ph(expr));
        }
        // A string written over two lines can't keep its line break inside
        // a string, so it doesn't pass through.
        assert_eq!(ok("{{\n'a' }}"), "kv\n      ");
    }

    #[test]
    fn whitespace_control_is_understood() {
        assert_eq!(ok("{%- if a -%}x{%- else -%}y{%- endif -%}").trim(), "x");
        let config = "{{- config(a=1) -}}";
        assert_eq!(ok(config), sp(config));
    }

    #[test]
    fn an_unterminated_tag_cannot_be_neutralised() {
        let src = "select {{ ref('a') }}\nfrom {{ ref('b'\nwhere 1";
        let f = neutralise(src).unwrap_err();
        assert_eq!(f.reason, Reason::UnterminatedTag { line: 2 });
        assert_eq!(f.reason.to_string(), "unterminated Jinja tag on line 2");
        assert_same_shape(src, &f.text);
        assert_eq!(
            f.text,
            format!(
                "select {}\nfrom {}\n{}",
                ph("{{ ref('a') }}"),
                sp("{{ ref('b'"),
                sp("where 1")
            )
        );
        // An unclosed comment and an unclosed string inside a tag too.
        for src in ["a {# b", "{{ 'x }}"] {
            assert!(matches!(
                neutralise(src).unwrap_err().reason,
                Reason::UnterminatedTag { line: 1 }
            ));
        }
    }

    #[test]
    fn a_raw_block_cannot_be_neutralised() {
        let (open, close) = ("{% raw %}", "{% endraw %}");
        let src = format!("select 1\n{open}{{{{ x }}}}{close} from t");
        let f = neutralise(&src).unwrap_err();
        assert_eq!(f.reason, Reason::RawBlock { line: 2 });
        assert_eq!(f.reason.to_string(), "{% raw %} block on line 2");
        assert_same_shape(&src, &f.text);
        // The body is kept: it is the literal text the block renders.
        assert_eq!(
            f.text,
            format!("select 1\n{}{{{{ x }}}}{} from t", sp(open), sp(close))
        );
    }

    #[test]
    fn multi_byte_text_inside_a_tag_keeps_the_byte_length() {
        let (expr, r) = ("{{ 'é—x' if a }}", "{{ ref('t') }}");
        assert_eq!(
            ok(&format!("select {expr} from {r}")),
            format!("select {} from {}", ph(expr), ph(r))
        );
        let one = "{{ 'é' }}";
        assert_eq!(ok(one), format!("é{}", " ".repeat(one.len() - "é".len())));
    }

    #[test]
    fn placeholders_are_recognised() {
        assert!(is_placeholder("kv___"));
        assert!(!is_placeholder("kv"));
        assert!(!is_placeholder("kvx_"));
        assert!(!is_placeholder("kv__a"));
    }

    /// A small deterministic generator, so the property test needs no new
    /// dependency.
    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// Property: whatever the input, neutralised or not, the output has the
    /// source's byte length and every line break in place.
    #[test]
    fn any_input_keeps_its_length_and_line_breaks() {
        const PIECES: &[&str] = &[
            "{{",
            "}}",
            "{%",
            "%}",
            "{#",
            "#}",
            "'",
            "\"",
            "\\",
            "\n",
            "\r\n",
            " ",
            "-",
            "+",
            "select ",
            "from ",
            "x",
            "ref('a')",
            "config(",
            ")",
            "if ",
            "elif ",
            "else ",
            "endif",
            "for c in l ",
            "endfor",
            "set y ",
            "set y = 1 ",
            "endset",
            "macro m() ",
            "endmacro",
            "raw",
            "endraw",
            "snapshot s ",
            "endsnapshot",
            "é",
            "—",
            "日本",
        ];
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        for _ in 0..20_000 {
            let n = (rng.next() % 40) as usize;
            let src: String = (0..n)
                .map(|_| PIECES[(rng.next() % PIECES.len() as u64) as usize])
                .collect();
            match neutralise(&src) {
                Ok(out) => assert_same_shape(&src, &out),
                Err(f) => assert_same_shape(&src, &f.text),
            }
        }
    }

    /// Property, on real models: every committed corpus model neutralises,
    /// keeps its byte length and every newline position, and leaves no tag.
    #[test]
    fn every_corpus_model_keeps_its_length_and_newlines() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sql_corpus");
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("manifest.json")).unwrap())
                .unwrap();
        let models = manifest["models"].as_array().unwrap();
        assert!(models.len() >= 16);
        let newlines = |s: &str| -> Vec<usize> { s.match_indices('\n').map(|(i, _)| i).collect() };
        for m in models {
            let path = root.join(m["path"].as_str().unwrap());
            let src = std::fs::read_to_string(&path).unwrap();
            let out =
                neutralise(&src).unwrap_or_else(|f| panic!("{}: {}", path.display(), f.reason));
            assert_same_shape(&src, &out);
            assert_eq!(newlines(&src), newlines(&out), "{}", path.display());
            assert!(
                !out.contains("{{") && !out.contains("{%"),
                "{}",
                path.display()
            );
        }
    }
}
