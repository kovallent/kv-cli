//! What a Jinja template writes down, as opposed to what it looks up.
//!
//! `{{ env_var('DBT_PASSWORD') }}` is a reference: the value arrives at render
//! time. `{{ env_var('DBT_PASSWORD', 's3cr3t') }}` is a reference with a
//! hardcoded fallback, and the fallback is as committed as a plain literal.
//! [`literals`] separates the two, so the caller can judge the written-down
//! parts with the same rules as any other value.

/// The literals a templated value writes down, or `None` when the value is
/// not a Jinja template at all.
///
/// A literal is:
/// - a quoted string that is the whole expression: `{{ 's3cr3t' }}`, filters
///   allowed (`{{ 's3cr3t' | trim }}`);
/// - the fallback of `env_var(name, fallback)` or `var(name, fallback)`, and
///   the argument of a `default` filter;
/// - text forming a whole branch between statements:
///   `{% if prod %}s3cr3t{% else %}…{% endif %}`.
///
/// Not literals: names looked up (`env_var('X')`), operands of comparisons in
/// `{% if %}`, comments, and text beside an expression (`Bearer {{ token }}`,
/// `prefix-{{ x }}`), which is a prefix to a reference rather than a value.
///
/// A value with only a stray closing `}}`, `%}` or `#}` is a broken
/// template; it holds no literal.
pub fn literals(value: &str) -> Option<Vec<String>> {
    if !["{{", "}}", "{%", "%}", "{#", "#}"]
        .iter()
        .any(|d| value.contains(d))
    {
        return None;
    }
    let mut out = Vec::new();
    // What bounded the text before the current position: a statement tag, or
    // anything else (the start of the value, an expression, a comment).
    let mut after_statement = false;
    let mut rest = value;
    while !rest.is_empty() {
        let next = ["{{", "{%", "{#"]
            .iter()
            .filter_map(|open| rest.find(open).map(|i| (i, *open)))
            .min();
        let Some((at, open)) = next else {
            break;
        };
        let text = &rest[..at];
        if open == "{%" && after_statement && !text.trim().is_empty() {
            out.push(text.trim().to_string());
        }
        let close = match open {
            "{{" => "}}",
            "{%" => "%}",
            _ => "#}",
        };
        let body_start = at + open.len();
        let (body, next_rest) = match rest[body_start..].find(close) {
            Some(end) => (
                &rest[body_start..body_start + end],
                &rest[body_start + end + close.len()..],
            ),
            None => (&rest[body_start..], ""),
        };
        if open == "{{" {
            expression_literals(body, &mut out);
        }
        after_statement = open == "{%";
        rest = next_rest;
    }
    Some(out)
}

/// Literals in the body of one `{{ … }}`.
fn expression_literals(body: &str, out: &mut Vec<String>) {
    let body = body
        .trim()
        .trim_start_matches(['-', '+'])
        .trim_end_matches(['-', '+']);
    let mut stages = split_top_level(body, '|').into_iter();
    let head = stages.next().unwrap_or_default().trim();
    if let Some(s) = whole_string(head) {
        out.push(s);
    } else if let Some((name, args)) = call(head) {
        if matches!(name, "env_var" | "var") {
            out.extend(args.iter().skip(1).filter_map(|a| whole_string(a.trim())));
        }
    }
    for filter in stages {
        if let Some((name, args)) = call(filter.trim()) {
            if matches!(name, "default" | "d") {
                out.extend(args.first().and_then(|a| whole_string(a.trim())));
            }
        }
    }
}

/// `'abc'` or `"abc"`, with backslash escapes, as the whole of `s`.
fn whole_string(s: &str) -> Option<String> {
    let quote = s.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let mut out = String::new();
    let mut chars = s[1..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            c if c == quote => return chars.as_str().is_empty().then_some(out),
            c => out.push(c),
        }
    }
    None
}

/// `name(arg, arg)` as the whole of `s`. An unclosed call yields the
/// arguments written so far.
fn call(s: &str) -> Option<(&str, Vec<&str>)> {
    let open = s.find('(')?;
    let name = s[..open].trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    let inner = &s[open + 1..];
    let inner = inner.strip_suffix(')').unwrap_or(inner);
    Some((name, split_top_level(inner, ',')))
}

/// Split on `sep` outside quotes and brackets.
fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut quote, mut escaped, mut start) = (0i32, None, false, 0);
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lits(v: &str) -> Vec<String> {
        literals(v).expect("a template")
    }

    #[test]
    fn plain_values_are_not_templates() {
        assert_eq!(literals("hunter2"), None);
        assert_eq!(literals("${DB_PASSWORD}"), None);
    }

    #[test]
    fn references_write_nothing_down() {
        for v in [
            "{{ env_var('DBT_PASSWORD') }}",
            "{{env_var(\"DBT_PASSWORD\")}}",
            "{{- env_var('DBT_PASSWORD') -}}",
            "{{ env_var('DBT_PASSWORD') | trim }}",
            "{{ var('db_password') }}",
            "{{ target.password }}",
            "prefix-{{ env_var('X') }}-suffix",
            "Bearer {{ env_var('TOKEN') }}",
            "{{ env_var('U') }}:{{ env_var('P') }}",
            "{% if target.name == 'prod' %}{{ env_var('A') }}{% else %}{{ env_var('B') }}{% endif %}",
            "{# s3cr3t #}",
        ] {
            assert!(lits(v).is_empty(), "{v}: {:?}", lits(v));
        }
    }

    #[test]
    fn fallbacks_and_written_values_are_literals() {
        assert_eq!(lits("{{ env_var('P', 's3cr3t') }}"), ["s3cr3t"]);
        assert_eq!(lits("{{ var(\"p\", \"s3cr3t\") }}"), ["s3cr3t"]);
        assert_eq!(lits("{{ 's3cr3t' }}"), ["s3cr3t"]);
        assert_eq!(lits("{{ 's3cr3t' | trim }}"), ["s3cr3t"]);
        assert_eq!(lits("{{ env_var('P') | default('s3cr3t') }}"), ["s3cr3t"]);
        assert_eq!(lits("{{ env_var('P', 'it\\'s') }}"), ["it's"]);
        assert_eq!(
            lits("{% if target.name == 'prod' %}s3cr3t{% else %}{{ env_var('P') }}{% endif %}"),
            ["s3cr3t"]
        );
    }

    #[test]
    fn broken_templates_hold_no_literal() {
        for v in [
            "{{ env_var('DBT_PASSWORD' }}",
            "{{ env_var('DBT_PASSWORD')",
            "env_var('DBT_PASSWORD') }}",
            "{% if x %}",
        ] {
            assert!(lits(v).is_empty(), "{v}: {:?}", lits(v));
        }
    }

    #[test]
    fn a_comma_inside_a_fallback_does_not_split_it() {
        assert_eq!(lits("{{ env_var('P', 'a,b|c') }}"), ["a,b|c"]);
    }
}
