//! Credential scanning for config files: YAML, `.env`, INI/`.cfg`, TOML and
//! JSON.
//!
//! Like [`crate::yamlscan`], these are line-oriented scanners rather than
//! full parsers, for the same reason: a finding needs a line number, and the
//! parsers that understand the formats do not report one. Each extracts the
//! `key = value` scalars that can carry a credential; everything else in a
//! file is still covered by the value-pattern pass, which reads raw text.

use crate::yamlscan::{self, Scalar};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
    Yaml,
    /// `.env`, `.env.*`, `*.env`: `KEY=value`, optionally `export`ed.
    Env,
    /// INI-style, including `setup.cfg`, `airflow.cfg`, `alembic.ini`.
    Ini,
    Toml,
    Json,
}

impl ConfigKind {
    /// Recognise a config file by name, or `None` for anything else.
    pub fn of(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_lowercase();
        if name == ".env" || name.starts_with(".env.") || name.ends_with(".env") {
            return Some(Self::Env);
        }
        match path.extension()?.to_string_lossy().to_lowercase().as_str() {
            "yml" | "yaml" => Some(Self::Yaml),
            "ini" | "cfg" => Some(Self::Ini),
            "toml" => Some(Self::Toml),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Yaml => "YAML",
            Self::Env => ".env",
            Self::Ini => "INI",
            Self::Toml => "TOML",
            Self::Json => "JSON",
        }
    }

    pub fn scalars(self, source: &str) -> Vec<Scalar> {
        match self {
            Self::Yaml => yamlscan::scalars(source),
            Self::Env => line_scalars(source, env_line),
            Self::Ini => line_scalars(source, ini_line),
            Self::Toml => line_scalars(source, toml_line),
            Self::Json => json_scalars(source),
        }
    }

    /// The comment on a line, where ignore markers live. JSON has none.
    pub fn comment_of(self, line: &str) -> &str {
        match self {
            Self::Yaml => yamlscan::comment_of(line),
            Self::Env | Self::Toml => hash_comment(line),
            Self::Ini => {
                let t = line.trim_start();
                if t.starts_with(';') || t.starts_with('#') {
                    t
                } else {
                    inline_comment(line, &[" ;", " #", "\t;", "\t#"])
                }
            }
            Self::Json => "",
        }
    }

    /// True when the value is resolved at load time rather than written down.
    pub fn is_templated(self, value: &str) -> bool {
        match self {
            Self::Yaml => yamlscan::is_templated(value),
            // configparser interpolation: `%(db_password)s`.
            Self::Ini => value.contains("${") || value.contains("%("),
            Self::Env | Self::Toml | Self::Json => value.contains("${") || value.contains("{{"),
        }
    }
}

/// A `.env` template - `.env.example`, `.env.sample`, `example.env` - whose
/// job is to hold placeholders such as `API_KEY=your-api-key-here`. The
/// key-name rule would flag every one of them; value patterns still run, so a
/// real credential pasted into a template is caught.
pub fn is_env_template(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name.split('.')
        .any(|part| matches!(part, "example" | "sample" | "template" | "tmpl" | "dist"))
}

fn line_scalars(source: &str, parse: fn(&str) -> Option<(String, String, &str)>) -> Vec<Scalar> {
    source
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let (key, value, comment) = parse(line)?;
            (!key.is_empty() && !value.is_empty()).then(|| Scalar {
                key,
                value,
                line: i + 1,
                comment: comment.to_string(),
            })
        })
        .collect()
}

fn is_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// A quoted value at the start of `rest`: its body and what follows the
/// closing quote. Backslash escapes are honoured inside double quotes only.
/// An unterminated quote takes the rest of the line.
fn quoted(rest: &str) -> Option<(String, &str)> {
    let q = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let mut escaped = false;
    for (i, c) in rest.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if c == '\\' && q == '"' {
            escaped = true;
        } else if c == q {
            return Some((rest[1..i].to_string(), &rest[i + 1..]));
        }
    }
    Some((rest[1..].to_string(), ""))
}

/// The comment starting at the first of `starts` (each a whitespace + marker
/// pair), or `""`.
fn inline_comment<'a>(line: &'a str, starts: &[&str]) -> &'a str {
    starts
        .iter()
        .filter_map(|s| line.find(s).map(|i| i + 1))
        .min()
        .map_or("", |i| &line[i..])
}

/// A `#` comment that is a whole line or follows whitespace, outside quotes.
fn hash_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && prev.is_whitespace() => return &line[i..],
            None => {}
        }
        prev = c;
    }
    ""
}

/// Split a value into its literal and trailing comment, unquoting if quoted.
fn value_and_comment<'a>(rest: &'a str, comment_of: fn(&'a str) -> &'a str) -> (String, &'a str) {
    let rest = rest.trim();
    match quoted(rest) {
        Some((body, after)) => (body, comment_of(after)),
        None => {
            let comment = comment_of(rest);
            let value = rest[..rest.len() - comment.len()].trim();
            (value.to_string(), comment)
        }
    }
}

/// `KEY=value`, `export KEY="value"  # comment`
fn env_line(line: &str) -> Option<(String, String, &str)> {
    let t = line.trim_start();
    if t.starts_with('#') {
        return None;
    }
    let t = t.strip_prefix("export ").unwrap_or(t);
    let (key, rest) = t.split_once('=')?;
    let key = key.trim();
    if !is_key(key) {
        return None;
    }
    let (value, comment) = value_and_comment(rest, hash_comment);
    Some((key.to_string(), value, comment))
}

/// `key = value` or `key: value`, in any section.
fn ini_line(line: &str) -> Option<(String, String, &str)> {
    let t = line.trim_start();
    if t.is_empty() || t.starts_with(['#', ';', '[']) {
        return None;
    }
    let at = t.find(['=', ':'])?;
    let key = t[..at].trim();
    if !is_key(key) {
        return None;
    }
    let (value, comment) = value_and_comment(&t[at + 1..], |s| {
        inline_comment(s, &[" ;", " #", "\t;", "\t#"])
    });
    Some((key.to_string(), value, comment))
}

/// `key = "value"`, with bare, quoted or dotted keys. Only string values are
/// returned: numbers, booleans, arrays and tables carry no credential the key
/// rule could judge, and multi-line strings are left to the value patterns.
fn toml_line(line: &str) -> Option<(String, String, &str)> {
    let t = line.trim_start();
    if t.is_empty() || t.starts_with(['#', '[']) {
        return None;
    }
    // Split at the first `=` outside quotes.
    let mut quote: Option<char> = None;
    let mut at = None;
    for (i, c) in t.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '=' => {
                at = Some(i);
                break;
            }
            None => {}
        }
    }
    let at = at?;
    let key: String = t[..at]
        .trim()
        .split('.')
        .map(|seg| seg.trim().trim_matches(['"', '\'']))
        .collect::<Vec<_>>()
        .join(".");
    let rest = t[at + 1..].trim_start();
    if rest.starts_with("\"\"\"") || rest.starts_with("'''") {
        return None;
    }
    let (value, after) = quoted(rest)?;
    Some((key, value, hash_comment(after)))
}

/// Every `"key": "string"` pair, found with a string-aware tokenizer so a
/// match can never begin inside another string. Minified JSON on one line
/// yields every pair on it. Escapes are kept as written: the key rule only
/// needs a name and a length, and the value patterns read raw text.
fn json_scalars(source: &str) -> Vec<Scalar> {
    // (start offset, body) of each string token, in order.
    let bytes = source.as_bytes();
    let mut strings: Vec<(usize, usize, &str)> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < bytes.len() && bytes[i] != b'"' {
            i += if bytes[i] == b'\\' { 2 } else { 1 };
        }
        let end = i.min(bytes.len());
        strings.push((start, end + 1, &source[start + 1..end]));
        i = end + 1;
    }

    let mut line = 1;
    let mut counted_to = 0;
    let mut out = Vec::new();
    for pair in strings.windows(2) {
        let ((k_start, k_end, key), (v_start, _, value)) = (pair[0], pair[1]);
        let between = &source[k_end.min(v_start)..v_start];
        if between.trim() != ":" {
            continue;
        }
        line += source[counted_to..k_start].matches('\n').count();
        counted_to = k_start;
        if !value.is_empty() {
            out.push(Scalar {
                key: key.to_string(),
                value: value.to_string(),
                line,
                comment: String::new(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(kind: ConfigKind, src: &str) -> Vec<(String, String, usize)> {
        kind.scalars(src)
            .into_iter()
            .map(|s| (s.key, s.value, s.line))
            .collect()
    }

    fn p(k: &str, v: &str, line: usize) -> (String, String, usize) {
        (k.to_string(), v.to_string(), line)
    }

    #[test]
    fn recognises_config_files_by_name() {
        for (name, kind) in [
            (".env", ConfigKind::Env),
            (".env.production", ConfigKind::Env),
            ("prod.env", ConfigKind::Env),
            ("airflow.cfg", ConfigKind::Ini),
            ("alembic.ini", ConfigKind::Ini),
            ("secrets.toml", ConfigKind::Toml),
            ("profiles.yml", ConfigKind::Yaml),
            ("creds.JSON", ConfigKind::Json),
        ] {
            assert_eq!(ConfigKind::of(Path::new(name)), Some(kind), "{name}");
        }
        assert_eq!(ConfigKind::of(Path::new("app.py")), None);
        assert_eq!(ConfigKind::of(Path::new("notes.ipynb")), None);
    }

    #[test]
    fn env_files() {
        let src = "# comment\nDB_PASSWORD=s3cr3t\nexport API_TOKEN=\"abc def\"  # note\nURL=https://x/#frag\nEMPTY=\nnot a pair\n";
        assert_eq!(
            pairs(ConfigKind::Env, src),
            vec![
                p("DB_PASSWORD", "s3cr3t", 2),
                p("API_TOKEN", "abc def", 3),
                p("URL", "https://x/#frag", 4),
            ]
        );
        assert_eq!(ConfigKind::Env.scalars(src)[1].comment, "# note");
    }

    #[test]
    fn env_templates_are_recognised() {
        for name in [
            ".env.example",
            ".env.sample",
            "example.env",
            ".env.template",
            ".env.dist",
        ] {
            assert!(is_env_template(Path::new(name)), "{name}");
        }
        assert!(!is_env_template(Path::new(".env")));
        assert!(!is_env_template(Path::new(".env.production")));
    }

    #[test]
    fn ini_files() {
        let src = "[core]\nfernet_key = abc123  ; rotate me\nsql_alchemy_conn: postgresql://u:p@h/db\n; password = commented\n    requests>=2.0\n";
        assert_eq!(
            pairs(ConfigKind::Ini, src),
            vec![
                p("fernet_key", "abc123", 2),
                p("sql_alchemy_conn", "postgresql://u:p@h/db", 3),
            ]
        );
        assert!(ConfigKind::Ini.is_templated("%(db_password)s"));
    }

    #[test]
    fn toml_files() {
        let src = "[db]\npassword = \"s3cr3t\" # inline\n\"api.key\" = 'lit'\nport = 5432\nnested.token = \"t0k\"\nblob = \"\"\"\nmulti\n\"\"\"\n";
        assert_eq!(
            pairs(ConfigKind::Toml, src),
            vec![
                p("password", "s3cr3t", 2),
                p("api.key", "lit", 3),
                p("nested.token", "t0k", 5),
            ]
        );
    }

    #[test]
    fn json_pairs_with_lines_including_minified() {
        let src = "{\n  \"user\": \"svc\",\n  \"password\": \"s3cr3t\",\n  \"n\": 1,\n  \"list\": [\"a\", \"b\"]\n}\n";
        assert_eq!(
            pairs(ConfigKind::Json, src),
            vec![p("user", "svc", 2), p("password", "s3cr3t", 3)]
        );
        let min = r#"{"a":{"api_key":"k1","x":"y"},"tok":"v"}"#;
        assert_eq!(
            pairs(ConfigKind::Json, min),
            vec![p("api_key", "k1", 1), p("x", "y", 1), p("tok", "v", 1)]
        );
    }

    /// A string containing `": "` must not be mistaken for a pair boundary.
    #[test]
    fn json_tokenizer_never_starts_inside_a_string() {
        let src = r#"{"note": "say \": \" here", "password": "s3cr3t"}"#;
        assert_eq!(
            pairs(ConfigKind::Json, src),
            vec![
                p("note", r#"say \": \" here"#, 1),
                p("password", "s3cr3t", 1)
            ]
        );
    }

    #[test]
    fn comments_per_format() {
        assert_eq!(ConfigKind::Env.comment_of("K=v # c"), "# c");
        assert_eq!(ConfigKind::Env.comment_of("K=\"a # b\""), "");
        assert_eq!(ConfigKind::Ini.comment_of("; whole"), "; whole");
        assert_eq!(ConfigKind::Toml.comment_of("k = \"v\"  # c"), "# c");
        assert_eq!(ConfigKind::Json.comment_of("\"k\": \"v # c\""), "");
    }
}
