//! Credential scanning for YAML config files.
//!
//! Aimed at dbt's `profiles.yml` / `dbt_project.yml`, where plaintext
//! warehouse passwords are the most common way credentials escape a dbt
//! project, but it applies to any scanned YAML.
//!
//! This is a line scanner, not a YAML parser: `serde_yaml` would give us the
//! structure but not the line numbers, and a finding without a line number is
//! not actionable. Only the shapes that can carry a secret are handled -
//! `key: value` scalars, including inside block sequences.

/// One `key: value` scalar found in a YAML document.
#[derive(Debug, Clone)]
pub struct Scalar {
    pub key: String,
    pub value: String,
    pub line: usize,
    /// The line's comment, if any, for ignore-marker checks. Only the comment
    /// counts: a marker inside the value is data, not intent.
    pub comment: String,
}

/// True when the value is supplied at render time rather than written down:
/// `{{ env_var('X') }}`, `${VAR}`, an application tag such as `!ENV x` or
/// `!vault |`, or an alias `*name` to a value defined (and judged) elsewhere.
///
/// Anchors (`&name value`) and core-schema tags (`!!str value`) are not in
/// this list: they decorate a literal written right there, and
/// [`strip_node_properties`] removes them before the value is judged.
pub fn is_templated(value: &str) -> bool {
    let v = value.trim();
    v.contains("{{") || v.contains("${") || v.starts_with('!') || v.starts_with('*')
}

/// Remove leading YAML node properties - anchors (`&pw`) and core-schema tags
/// (`!!str`), in either order - leaving the value they decorate.
///
/// These used to be treated as "templated" and skipped, so
/// `password: &pw s3cr3t` and `password: !!str s3cr3t` passed unexamined.
/// Application tags (a single `!`) are left in place: those name a resolver.
fn strip_node_properties(value: &str) -> &str {
    let mut v = value.trim_start();
    loop {
        let is_property = v.starts_with('&') || v.starts_with("!!");
        if !is_property {
            return v;
        }
        let end = v.find(char::is_whitespace).unwrap_or(v.len());
        v = v[end..].trim_start();
    }
}

/// A value that opens a literal/folded block. Everything indented under it is
/// free text, not mapping keys.
fn opens_literal_block(value: &str) -> bool {
    let v = value.trim();
    // Also covers explicit indentation indicators such as `|2-`.
    (v.starts_with('|') || v.starts_with('>'))
        && v[1..]
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == '+')
}

/// An empty value introduces a nested mapping or sequence: nothing to report
/// on this line, but the lines below it are still structure.
fn is_empty_value(value: &str) -> bool {
    value.trim().is_empty()
}

/// The inline `#` comment on a line, or `""`.
pub fn comment_of(line: &str) -> &str {
    split_comment(line).1
}

/// Split a line into its content and its inline `#` comment (empty if none).
fn split_comment(line: &str) -> (&str, &str) {
    let content = strip_comment(line);
    (content, &line[content.len()..])
}

/// Strip an inline `#` comment that is not inside quotes.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            // A `#` only starts a comment at the start of a line or after
            // whitespace, so `https://x/#frag` stays intact.
            None if b == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) => {
                return &line[..i];
            }
            None => {}
        }
    }
    line
}

fn unquote(value: &str) -> String {
    let v = value.trim();
    let bytes = v.as_bytes();
    if v.len() >= 2 {
        let (first, last) = (bytes[0], bytes[v.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

fn push_flow_pairs(text: &str, line: usize, comment: &str, out: &mut Vec<Scalar>) {
    for (key, value) in flow_pairs(text).unwrap_or_default() {
        out.push(Scalar {
            key,
            value,
            line,
            comment: comment.to_string(),
        });
    }
}

/// The `key: value` pairs of a flow mapping written on one line, nested
/// mappings included. Sequences hold no keys and are skipped. A flow mapping
/// continued onto later lines yields nothing here; the value patterns still
/// read it.
fn flow_pairs(text: &str) -> Option<Vec<(String, String)>> {
    let inner = text.trim().strip_prefix('{')?.strip_suffix('}')?;
    let mut out = Vec::new();
    for part in split_top_level(inner, ',') {
        let Some(at) = split_top_level(part, ':').first().map(|k| k.len()) else {
            continue;
        };
        if at >= part.len() {
            continue;
        }
        let key = unquote(&part[..at]);
        let value = strip_node_properties(part[at + 1..].trim());
        if value.starts_with('{') {
            out.extend(flow_pairs(value).unwrap_or_default());
            continue;
        }
        if key.is_empty() || key.contains(char::is_whitespace) || value.starts_with('[') {
            continue;
        }
        let value = unquote(value);
        if !value.is_empty() {
            out.push((key, value));
        }
    }
    Some(out)
}

/// Split on `sep` where it is outside quotes and nested brackets.
fn split_top_level(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut quote, mut start) = (0i32, None::<char>, 0);
    for (i, c) in text.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                '"' | '\'' => quote = Some(c),
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                _ if c == sep && depth == 0 => {
                    parts.push(&text[start..i]);
                    start = i + c.len_utf8();
                }
                _ => {}
            },
        }
    }
    parts.push(&text[start..]);
    parts
}

/// Extract every `key: value` scalar from a YAML document.
pub fn scalars(source: &str) -> Vec<Scalar> {
    let mut out = Vec::new();
    // Indent of the key that opened a literal block, while inside one.
    let mut block: Option<usize> = None;

    for (idx, raw_line) in source.lines().enumerate() {
        let indent = raw_line.len() - raw_line.trim_start().len();
        if let Some(open_at) = block {
            // Blank lines and anything more indented belong to the block.
            if raw_line.trim().is_empty() || indent > open_at {
                continue;
            }
            block = None;
        }

        let (line, comment) = split_comment(raw_line);
        let mut rest = line.trim_start();

        // Block sequence entries: `- key: value`, possibly nested `- - `.
        while let Some(stripped) = rest.strip_prefix('-') {
            if stripped.starts_with(' ') || stripped.is_empty() {
                rest = stripped.trim_start();
            } else {
                break;
            }
        }
        if rest.is_empty() || rest.starts_with('#') {
            continue;
        }
        // `- {user: svc, password: x}`: a flow mapping as the whole entry.
        if rest.starts_with('{') {
            push_flow_pairs(rest, idx + 1, comment, &mut out);
            continue;
        }

        // Split on the first `:` that is followed by whitespace or ends the
        // line, so `url: https://x` does not split at the scheme colon.
        let bytes = rest.as_bytes();
        let mut split = None;
        let mut quote: Option<u8> = None;
        for (i, &b) in bytes.iter().enumerate() {
            match quote {
                Some(q) if b == q => quote = None,
                Some(_) => {}
                None if b == b'"' || b == b'\'' => quote = Some(b),
                None if b == b':' => {
                    let next = bytes.get(i + 1);
                    if next.is_none() || next.is_some_and(|c| c.is_ascii_whitespace()) {
                        split = Some(i);
                        break;
                    }
                }
                None => {}
            }
        }
        let Some(at) = split else { continue };

        let key = unquote(&rest[..at]);
        let value = strip_node_properties(&rest[at + 1..]);
        // A key containing whitespace is prose, not a mapping key.
        if key.is_empty() || key.contains(char::is_whitespace) {
            continue;
        }
        if opens_literal_block(value) {
            block = Some(indent);
            continue;
        }
        if is_empty_value(value) {
            continue;
        }
        // `creds: {user: svc, password: x}`: judge the pairs, not the braces.
        if value.starts_with('{') {
            push_flow_pairs(value, idx + 1, comment, &mut out);
            continue;
        }

        out.push(Scalar {
            key,
            value: unquote(value),
            line: idx + 1,
            comment: comment.to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(s: &'a [Scalar], key: &str) -> Option<&'a Scalar> {
        s.iter().find(|x| x.key == key)
    }

    #[test]
    fn extracts_nested_scalars_with_line_numbers() {
        let src =
            "prod:\n  outputs:\n    default:\n      type: snowflake\n      password: hunter2\n";
        let s = scalars(src);
        let p = find(&s, "password").unwrap();
        assert_eq!(p.value, "hunter2");
        assert_eq!(p.line, 5);
        assert_eq!(find(&s, "type").unwrap().value, "snowflake");
    }

    #[test]
    fn recognises_templated_values() {
        let s = scalars("password: \"{{ env_var('DBT_PASSWORD') }}\"\n");
        assert!(is_templated(&find(&s, "password").unwrap().value));
        assert!(is_templated("${DB_PASS}"));
        assert!(!is_templated("hunter2"));
    }

    #[test]
    fn ignores_comments_and_block_openers() {
        let s = scalars("# password: hunter2\nreal: yes\nnotes: |\n  password: not-a-secret\n");
        assert!(find(&s, "password").is_none());
        assert_eq!(find(&s, "real").unwrap().value, "yes");
    }

    #[test]
    fn strips_inline_comments_but_not_urls_or_hashes_in_quotes() {
        let s = scalars("token: abc123  # set me\nurl: https://x/#frag\nk: \"a # b\"\n");
        assert_eq!(find(&s, "token").unwrap().value, "abc123");
        assert_eq!(find(&s, "url").unwrap().value, "https://x/#frag");
        assert_eq!(find(&s, "k").unwrap().value, "a # b");
    }

    #[test]
    fn handles_sequence_entries() {
        let s = scalars("targets:\n  - name: prod\n    password: hunter2\n");
        assert_eq!(find(&s, "name").unwrap().value, "prod");
        assert_eq!(find(&s, "password").unwrap().value, "hunter2");
    }

    #[test]
    fn does_not_split_urls_at_the_scheme_colon() {
        let s = scalars("host: jdbc:postgresql://db:5432/prod\n");
        assert_eq!(
            find(&s, "host").unwrap().value,
            "jdbc:postgresql://db:5432/prod"
        );
    }

    #[test]
    fn skips_prose_lines_inside_block_scalars() {
        let s =
            scalars("desc: |\n  this line: has a colon but is prose\n  password: not-a-secret\n");
        assert!(find(&s, "desc").is_none());
        assert!(find(&s, "password").is_none());
    }

    #[test]
    fn resumes_parsing_after_a_block_scalar_ends() {
        let src = "notes: >-\n  wrapped text\n\n  more text\npassword: hunter2\n";
        let s = scalars(src);
        assert!(find(&s, "notes").is_none());
        let p = find(&s, "password").unwrap();
        assert_eq!(p.value, "hunter2");
        assert_eq!(p.line, 5);
    }

    /// Regression: anchors and core tags were treated as "templated" and the
    /// literal behind them skipped.
    #[test]
    fn anchors_and_core_tags_do_not_hide_the_value() {
        for src in [
            "password: &pw hunter2\n",
            "password: !!str hunter2\n",
            "password: &pw !!str hunter2\n",
            "password: !!str &pw \"hunter2\"\n",
        ] {
            let s = scalars(src);
            let p = find(&s, "password").unwrap_or_else(|| panic!("{src}"));
            assert_eq!(p.value, "hunter2", "{src}");
            assert!(!is_templated(&p.value), "{src}");
        }
        // An anchor on a block opener still opens the block.
        assert!(find(&scalars("notes: &n |\n  password: prose\n"), "password").is_none());
    }

    #[test]
    fn application_tags_and_aliases_still_count_as_supplied_elsewhere() {
        assert!(is_templated("!ENV ${DB_PASSWORD}"));
        assert!(is_templated("!vault |"));
        assert!(is_templated("*pw"));
    }

    #[test]
    fn only_the_comment_is_kept_for_marker_checks() {
        let s = scalars(
            "password: \"kovallent:allow-secret\"\ntoken: abc123  # kovallent:allow-secret\n",
        );
        assert!(!find(&s, "password").unwrap().comment.contains("kovallent"));
        assert!(find(&s, "token")
            .unwrap()
            .comment
            .contains("kovallent:allow-secret"));
    }

    /// Regression: a flow mapping was one scalar - `{password: hunter2}` -
    /// under the outer key, so the inner key was never judged.
    #[test]
    fn flow_mapping_pairs_are_scalars_too() {
        let s = scalars("creds: {user: svc, password: hunter2}\n");
        assert_eq!(find(&s, "password").unwrap().value, "hunter2");
        assert_eq!(find(&s, "user").unwrap().value, "svc");
        assert!(find(&s, "creds").is_none());

        let s = scalars("targets:\n  - {name: prod, auth: {token: \"t0k3n, with comma\"}}\n");
        assert_eq!(find(&s, "token").unwrap().value, "t0k3n, with comma");
        assert_eq!(find(&s, "token").unwrap().line, 2);

        let s = scalars("x: {hosts: [a, b], url: \"http://h:1/p\"}\n");
        assert!(find(&s, "hosts").is_none());
        assert_eq!(find(&s, "url").unwrap().value, "http://h:1/p");
    }

    #[test]
    fn nested_mappings_are_still_traversed() {
        // An empty value opens structure, not a literal block.
        let s = scalars("outputs:\n  prod:\n    password: hunter2\n");
        assert_eq!(find(&s, "password").unwrap().value, "hunter2");
    }
}
