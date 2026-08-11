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
pub struct YamlScalar {
    pub key: String,
    pub value: String,
    pub line: usize,
    /// Whole line, for ignore-marker checks.
    pub raw: String,
}

/// True when the value is supplied at render time rather than written down:
/// `{{ env_var('X') }}`, `${VAR}`, `!ENV x`, an anchor/alias, or a tag.
pub fn is_templated(value: &str) -> bool {
    let v = value.trim();
    v.contains("{{")
        || v.contains("${")
        || v.starts_with('!')
        || v.starts_with('*')
        || v.starts_with('&')
        || v.starts_with("<<")
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

/// Extract every `key: value` scalar from a YAML document.
pub fn scalars(source: &str) -> Vec<YamlScalar> {
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

        let line = strip_comment(raw_line);
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
        let value = &rest[at + 1..];
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

        out.push(YamlScalar {
            key,
            value: unquote(value),
            line: idx + 1,
            raw: raw_line.to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(s: &'a [YamlScalar], key: &str) -> Option<&'a YamlScalar> {
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

    #[test]
    fn nested_mappings_are_still_traversed() {
        // An empty value opens structure, not a literal block.
        let s = scalars("outputs:\n  prod:\n    password: hunter2\n");
        assert_eq!(find(&s, "password").unwrap().value, "hunter2");
    }
}
