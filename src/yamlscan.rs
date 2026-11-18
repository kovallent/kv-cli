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

use std::path::Path;

/// One `key: value` scalar found in a YAML document.
#[derive(Debug, Clone)]
pub struct Scalar {
    pub key: String,
    pub value: String,
    pub line: usize,
    /// The line's comment, if any, for ignore-marker checks. Only the comment
    /// counts: a marker inside the value is data, not intent.
    pub comment: String,
    /// The keys of the mappings this scalar is nested in, outermost first:
    /// `["jobs", "build", "permissions"]` for a job's permission. Sequence
    /// entries add no key. Empty for formats other than YAML.
    pub parents: Vec<String>,
}

/// True when `path` is a GitHub Actions workflow: a `.yml` or `.yaml` file
/// directly inside a `.github/workflows` folder.
pub fn is_workflow_file(path: &Path) -> bool {
    let mut parts = path.components().rev().map(|c| c.as_os_str());
    let (Some(file), Some(dir), Some(github)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let yaml = Path::new(file)
        .extension()
        .is_some_and(|e| e == "yml" || e == "yaml");
    yaml && dir == "workflows" && github == ".github"
}

/// True when `scalar` is a GitHub Actions permission set to one of the three
/// values GitHub defines for it - `read`, `write` or `none`, in any case -
/// under `permissions:` at workflow level or in a job.
///
/// None of those values can be a credential, yet `id-token: write` matches the
/// default key pattern `*token*`. Any other value under `permissions` is not
/// a permission GitHub accepts, and is judged like any other scalar.
pub fn is_workflow_permission(scalar: &Scalar) -> bool {
    let parents: Vec<&str> = scalar.parents.iter().map(String::as_str).collect();
    let under_permissions = matches!(
        parents.as_slice(),
        ["permissions"] | ["jobs", _, "permissions"]
    );
    under_permissions
        && matches!(
            scalar.value.trim().to_ascii_lowercase().as_str(),
            "read" | "write" | "none"
        )
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

/// The scalar a quoted value spells: `''` is one quote inside single quotes;
/// `\"` and `\\` are escapes inside double quotes.
fn unquote(value: &str) -> String {
    let v = value.trim();
    let bytes = v.as_bytes();
    if v.len() >= 2 {
        let (first, last) = (bytes[0], bytes[v.len() - 1]);
        let inner = &v[1..v.len() - 1];
        if first == b'\'' && last == b'\'' {
            return inner.replace("''", "'");
        }
        if first == b'"' && last == b'"' {
            let mut out = String::with_capacity(inner.len());
            let mut chars = inner.chars();
            while let Some(c) = chars.next() {
                match (c, chars.clone().next()) {
                    ('\\', Some(next @ ('"' | '\\'))) => {
                        out.push(next);
                        chars.next();
                    }
                    _ => out.push(c),
                }
            }
            return out;
        }
    }
    v.to_string()
}

fn push_flow_pairs(
    text: &str,
    line: usize,
    comment: &str,
    parents: &[String],
    out: &mut Vec<Scalar>,
) {
    for (inner, key, value) in flow_pairs(text).unwrap_or_default() {
        out.push(Scalar {
            key,
            value,
            line,
            comment: comment.to_string(),
            parents: parents.iter().cloned().chain(inner).collect(),
        });
    }
}

/// The `key: value` pairs of a flow mapping written on one line, nested
/// mappings included, each with the keys of the flow mappings it is nested in
/// within this one. Sequences hold no keys and are skipped. A flow mapping
/// continued onto later lines yields nothing here; the value patterns still
/// read it.
fn flow_pairs(text: &str) -> Option<Vec<(Vec<String>, String, String)>> {
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
            for (mut inner, k, v) in flow_pairs(value).unwrap_or_default() {
                inner.insert(0, key.clone());
                out.push((inner, k, v));
            }
            continue;
        }
        if key.is_empty() || key.contains(char::is_whitespace) || value.starts_with('[') {
            continue;
        }
        let value = unquote(value);
        if !value.is_empty() {
            out.push((Vec::new(), key, value));
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
    // The block mappings enclosing the current line: each key that opened
    // one, with the column that key starts at.
    let mut open: Vec<(usize, String)> = Vec::new();

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
        // The column the line's content starts at, past any `- ` markers.
        let mut column = indent;

        // Block sequence entries: `- key: value`, possibly nested `- - `.
        while let Some(stripped) = rest.strip_prefix('-') {
            if stripped.starts_with(' ') || stripped.is_empty() {
                let entry = stripped.trim_start();
                column += rest.len() - entry.len();
                rest = entry;
            } else {
                break;
            }
        }
        if rest.is_empty() || rest.starts_with('#') {
            continue;
        }
        // Content at or left of a mapping's key closes that mapping.
        while open.last().is_some_and(|(at, _)| *at >= column) {
            open.pop();
        }
        let parents: Vec<String> = open.iter().map(|(_, key)| key.clone()).collect();
        if let Some((key, value)) = jinja_set(rest) {
            out.push(Scalar {
                key,
                value,
                line: idx + 1,
                comment: comment.to_string(),
                parents,
            });
            continue;
        }
        // `- {user: svc, password: x}`: a flow mapping as the whole entry.
        if rest.starts_with('{') {
            push_flow_pairs(rest, idx + 1, comment, &parents, &mut out);
            continue;
        }

        let Some(at) = key_colon(rest) else { continue };

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
            open.push((column, key));
            continue;
        }
        // `creds: {user: svc, password: x}`: judge the pairs, not the braces.
        // An unquoted `{{ … }}` or `{% … %}` is a template - invalid YAML,
        // but a person reads it as the value, credential and all.
        if value.starts_with('{') && !value.starts_with("{{") && !value.starts_with("{%") {
            let mut within = parents;
            within.push(key);
            push_flow_pairs(value, idx + 1, comment, &within, &mut out);
            continue;
        }

        out.push(Scalar {
            key,
            value: unquote(value),
            line: idx + 1,
            comment: comment.to_string(),
            parents,
        });
    }
    out
}

/// The `:` that ends a mapping key: the first one outside quotes that is
/// followed by whitespace or ends the line, so `url: https://x` does not
/// split at the scheme colon.
fn key_colon(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b':' => {
                let next = bytes.get(i + 1);
                if next.is_none() || next.is_some_and(|c| c.is_ascii_whitespace()) {
                    return Some(i);
                }
            }
            None => {}
        }
    }
    None
}

/// One step of a path into a YAML document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Key(String),
    Index(usize),
}

/// A scalar and its full path, for files where the place a value sits
/// decides what it means: in a Databricks asset bundle, the same key under
/// `targets.dev` is a per-target value, and anywhere else it is pinned.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: Vec<Segment>,
    pub value: String,
    pub line: usize,
    /// The line's comment, for ignore-marker checks.
    pub comment: String,
}

impl Entry {
    /// The mapping key this scalar is the value of; `None` for a sequence
    /// item.
    pub fn key(&self) -> Option<&str> {
        match self.path.last() {
            Some(Segment::Key(k)) => Some(k),
            _ => None,
        }
    }

    /// The first key of the path: the top-level section the scalar is in.
    pub fn section(&self) -> Option<&str> {
        match self.path.first() {
            Some(Segment::Key(k)) => Some(k),
            _ => None,
        }
    }

    /// `resources.jobs.nightly.job_clusters[0].new_cluster.cluster_id`.
    pub fn dotted(&self) -> String {
        let mut out = String::new();
        for s in &self.path {
            match s {
                Segment::Key(k) => {
                    if !out.is_empty() {
                        out.push('.');
                    }
                    out.push_str(k);
                }
                Segment::Index(i) => out.push_str(&format!("[{i}]")),
            }
        }
        out
    }
}

/// Every scalar in a YAML document with its full path, sequence entries
/// included: mapping values, block-sequence items, and the members of flow
/// mappings and flow sequences written on one line. Literal and folded
/// blocks are skipped as free text, as in [`scalars`].
///
/// Like [`scalars`], a line scanner rather than a parser, so each scalar keeps
/// its line number. Indentation decides nesting; a sequence may sit at the
/// same column as the key that owns it.
pub fn entries(source: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    // Column of the key that opened a literal block, while inside one.
    let mut block: Option<usize> = None;
    // The containers enclosing the current line, outermost first: a key whose
    // value is a nested block, or a block-sequence entry, with its column.
    let mut open: Vec<(usize, Segment)> = Vec::new();
    let prefix = |open: &[(usize, Segment)]| -> Vec<Segment> {
        open.iter().map(|(_, s)| s.clone()).collect()
    };

    for (idx, raw_line) in source.lines().enumerate() {
        let indent = raw_line.len() - raw_line.trim_start().len();
        if let Some(open_at) = block {
            if raw_line.trim().is_empty() || indent > open_at {
                continue;
            }
            block = None;
        }
        let (line, comment) = split_comment(raw_line);
        let mut rest = line.trim_start();
        if rest.is_empty() {
            continue;
        }
        if indent == 0 && (rest == "---" || rest.starts_with("--- ") || rest == "...") {
            open.clear();
            continue;
        }

        // Each `- ` opens a sequence entry at its column, or moves to the
        // next entry of the sequence already open there.
        let mut column = indent;
        let mut in_entry = false;
        while let Some(stripped) = rest.strip_prefix('-') {
            if !(stripped.starts_with(' ') || stripped.is_empty()) {
                break;
            }
            let dash = column;
            while open.last().is_some_and(|(at, _)| *at > dash) {
                open.pop();
            }
            match open.last_mut() {
                Some((at, Segment::Index(i))) if *at == dash => *i += 1,
                _ => open.push((dash, Segment::Index(0))),
            }
            in_entry = true;
            let entry = stripped.trim_start();
            column += rest.len() - entry.len();
            rest = entry;
        }
        if rest.is_empty() {
            continue;
        }

        let push = |out: &mut Vec<Entry>, path: Vec<Segment>, value: &str| {
            let value = unquote(value);
            if !value.is_empty() {
                out.push(Entry {
                    path,
                    value,
                    line: idx + 1,
                    comment: comment.to_string(),
                });
            }
        };
        // A value on this line: a scalar, or a flow collection written out.
        let emit = |out: &mut Vec<Entry>, path: Vec<Segment>, value: &str| {
            if value.starts_with('{') && !value.starts_with("{{") && !value.starts_with("{%") {
                for (inner, k, v) in flow_pairs(value).unwrap_or_default() {
                    let mut p = path.clone();
                    p.extend(inner.into_iter().map(Segment::Key));
                    p.push(Segment::Key(k));
                    push(out, p, &v);
                }
            } else if let Some(items) = value
                .strip_prefix('[')
                .and_then(|v| v.trim_end().strip_suffix(']'))
            {
                for (i, item) in split_top_level(items, ',').into_iter().enumerate() {
                    let item = strip_node_properties(item.trim());
                    let mut p = path.clone();
                    p.push(Segment::Index(i));
                    if item.starts_with('{') {
                        for (inner, k, v) in flow_pairs(item).unwrap_or_default() {
                            let mut q = p.clone();
                            q.extend(inner.into_iter().map(Segment::Key));
                            q.push(Segment::Key(k));
                            push(out, q, &v);
                        }
                    } else if !item.starts_with('[') {
                        push(out, p, item);
                    }
                }
            } else {
                push(out, path, value);
            }
        };

        let key = key_colon(rest)
            .map(|at| (unquote(&rest[..at]), at))
            .filter(|(k, _)| !k.is_empty() && !k.contains(char::is_whitespace));
        let Some((key, at)) = key else {
            // A bare scalar is only a value as a sequence entry; anything
            // else is the continuation of a multi-line scalar.
            if in_entry {
                let value = strip_node_properties(rest);
                if opens_literal_block(value) {
                    block = Some(column);
                } else {
                    emit(&mut out, prefix(&open), value);
                }
            }
            continue;
        };

        // A key closes every container at or right of its column.
        while open.last().is_some_and(|(c, _)| *c >= column) {
            open.pop();
        }
        let value = strip_node_properties(&rest[at + 1..]);
        if opens_literal_block(value) {
            block = Some(column);
            continue;
        }
        if is_empty_value(value) {
            open.push((column, Segment::Key(key)));
            continue;
        }
        let mut path = prefix(&open);
        path.push(Segment::Key(key));
        emit(&mut out, path, value.trim_end());
    }
    out
}

/// `{% set name = expr %}` on one line, read as `name: {{ expr }}`, so the
/// expression is judged like any other template: a literal it writes down is
/// a value, a lookup is not. Tuple targets and `{% set %}…{% endset %}` blocks
/// are left alone.
fn jinja_set(line: &str) -> Option<(String, String)> {
    let body = line.strip_prefix("{%")?.trim_start_matches(['-', '+']);
    let body = body[..body.find("%}")?].trim_end_matches(['-', '+']).trim();
    let assignment = body.strip_prefix("set")?;
    if !assignment.starts_with(char::is_whitespace) {
        return None;
    }
    let (name, expr) = assignment.split_once('=')?;
    let name = name.trim();
    let expr = expr.trim();
    let identifier = !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_');
    (identifier && !expr.is_empty()).then(|| (name.to_string(), format!("{{{{ {expr} }}}}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dotted(src: &str) -> Vec<(String, String, usize)> {
        entries(src)
            .into_iter()
            .map(|e| (e.dotted(), e.value, e.line))
            .collect()
    }

    /// Mapping keys, block sequences (indented or at the key's own column),
    /// bare sequence items and flow collections all get their full path.
    #[test]
    fn entries_carry_their_full_path() {
        let src = "\
resources:
  jobs:
    nightly:
      job_clusters:
        - job_cluster_key: main
          new_cluster:
            cluster_id: 1234-567890-abcd1234
      tasks:
      - task_key: run
        spark_python_task:
          parameters:
            - --input
            - \"s3://prod-orders/raw/\"  # a comment
      - task_key: second
        tags: {team: data, env: prod}
        args: [one, 'two', {k: v}]
other: 1
";
        assert_eq!(
            dotted(src),
            [
                (
                    "resources.jobs.nightly.job_clusters[0].job_cluster_key".into(),
                    "main".into(),
                    5
                ),
                (
                    "resources.jobs.nightly.job_clusters[0].new_cluster.cluster_id".into(),
                    "1234-567890-abcd1234".into(),
                    7
                ),
                (
                    "resources.jobs.nightly.tasks[0].task_key".into(),
                    "run".into(),
                    9
                ),
                (
                    "resources.jobs.nightly.tasks[0].spark_python_task.parameters[0]".into(),
                    "--input".into(),
                    12
                ),
                (
                    "resources.jobs.nightly.tasks[0].spark_python_task.parameters[1]".into(),
                    "s3://prod-orders/raw/".into(),
                    13
                ),
                (
                    "resources.jobs.nightly.tasks[1].task_key".into(),
                    "second".into(),
                    14
                ),
                (
                    "resources.jobs.nightly.tasks[1].tags.team".into(),
                    "data".into(),
                    15
                ),
                (
                    "resources.jobs.nightly.tasks[1].tags.env".into(),
                    "prod".into(),
                    15
                ),
                (
                    "resources.jobs.nightly.tasks[1].args[0]".into(),
                    "one".into(),
                    16
                ),
                (
                    "resources.jobs.nightly.tasks[1].args[1]".into(),
                    "two".into(),
                    16
                ),
                (
                    "resources.jobs.nightly.tasks[1].args[2].k".into(),
                    "v".into(),
                    16
                ),
                ("other".into(), "1".into(), 17),
            ]
        );
        let e = &entries(src)[4];
        assert_eq!(e.key(), None);
        assert_eq!(e.section(), Some("resources"));
        assert_eq!(e.comment.trim(), "# a comment");
    }

    /// A literal block is free text, and the keys after it are structure
    /// again; a new document starts from the top.
    #[test]
    fn entries_skip_literal_blocks_and_reset_per_document() {
        let src = "a:\n  script: |\n    cluster_id: not-a-key\n  after: 1\n---\nb: 2\n";
        assert_eq!(
            dotted(src),
            [
                ("a.after".into(), "1".into(), 4),
                ("b".into(), "2".into(), 6)
            ]
        );
    }

    #[test]
    fn quoted_scalars_are_unescaped() {
        assert_eq!(unquote("'it''s'"), "it's");
        assert_eq!(unquote(r#""say \"hi\" \\ bye""#), r#"say "hi" \ bye"#);
        assert_eq!(unquote(r#""C:\temp""#), r#"C:\temp"#);
    }

    /// Unquoted Jinja is invalid YAML, but it is a value, not a flow mapping.
    #[test]
    fn unquoted_jinja_is_one_scalar() {
        let s = scalars("password: {{ env_var('P', 'x') }}\n");
        assert_eq!(
            find(&s, "password").unwrap().value,
            "{{ env_var('P', 'x') }}"
        );
    }

    /// A Jinja assignment is a key and a template, so the template rules
    /// decide whether it holds a literal.
    #[test]
    fn jinja_set_lines_are_scalars() {
        let s = scalars("{% set db_password = 's3cr3t' %}\n  {%- set token = env_var('T') -%}\n");
        assert_eq!(find(&s, "db_password").unwrap().value, "{{ 's3cr3t' }}");
        assert_eq!(find(&s, "db_password").unwrap().line, 1);
        assert_eq!(find(&s, "token").unwrap().value, "{{ env_var('T') }}");
        for src in [
            "{% set a, b = 'x', 'y' %}\n",
            "{% set body %}\n",
            "{% setting = 'x' %}\n",
            "{% if x == 'y' %}\n",
        ] {
            assert!(scalars(src).is_empty(), "{src}");
        }
    }

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

    /// Each scalar knows the mappings it sits in; sequence entries add no
    /// key, and a comment line closes nothing.
    #[test]
    fn scalars_know_the_mappings_they_are_nested_in() {
        let src = "\
permissions:
  contents: read
  # a comment at the mapping's own indent
  id-token: write
jobs:
  build:
    permissions:
      checks: write
    steps:
      - uses: actions/checkout@v4
        with:
          token: abc
top: level
";
        let s = scalars(src);
        assert_eq!(find(&s, "contents").unwrap().parents, ["permissions"]);
        assert_eq!(find(&s, "id-token").unwrap().parents, ["permissions"]);
        assert_eq!(
            find(&s, "checks").unwrap().parents,
            ["jobs", "build", "permissions"]
        );
        assert_eq!(
            find(&s, "uses").unwrap().parents,
            ["jobs", "build", "steps"]
        );
        assert_eq!(
            find(&s, "token").unwrap().parents,
            ["jobs", "build", "steps", "with"]
        );
        assert!(find(&s, "top").unwrap().parents.is_empty());

        let s = scalars("permissions: {id-token: write, nested: {k: v}}\n");
        assert_eq!(find(&s, "id-token").unwrap().parents, ["permissions"]);
        assert_eq!(find(&s, "k").unwrap().parents, ["permissions", "nested"]);
    }

    #[test]
    fn workflow_files_are_recognised_by_location() {
        for path in [
            ".github/workflows/ci.yml",
            ".github/workflows/ci.yaml",
            "sub/.github/workflows/ci.yml",
            "/abs/repo/.github/workflows/kovallent.yml",
        ] {
            assert!(is_workflow_file(Path::new(path)), "{path}");
        }
        for path in [
            "config/app.yml",
            ".github/ci.yml",
            ".github/workflows/nested/ci.yml",
            ".github/workflows/ci.json",
            "workflows/ci.yml",
            "ci.yml",
        ] {
            assert!(!is_workflow_file(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn workflow_permissions_are_the_three_values_at_workflow_or_job_level() {
        let perm =
            |src: &str| -> Vec<bool> { scalars(src).iter().map(is_workflow_permission).collect() };
        assert_eq!(
            perm("permissions:\n  id-token: write\n  contents: READ\n  pages: None\n"),
            [true, true, true]
        );
        assert_eq!(
            perm("jobs:\n  b:\n    permissions:\n      id-token: \"write\"\n"),
            [true]
        );
        // Not one of the three values: judged like any other scalar.
        assert_eq!(
            perm("permissions:\n  id-token: hunter2hunter2\n  contents: write-all\n"),
            [false, false]
        );
        // `permissions` anywhere else is not GitHub's permissions block.
        assert_eq!(
            perm("jobs:\n  b:\n    steps:\n      - with:\n          permissions:\n            id-token: write\n"),
            [false]
        );
        assert_eq!(
            perm("deploy:\n  permissions:\n    id-token: write\n"),
            [false]
        );
    }
}
