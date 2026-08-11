//! Structural analysis of Python source, backed by tree-sitter.
//!
//! Everything the contract engine needs comes from the parse tree rather than
//! from text heuristics: function signatures (including multi-line ones,
//! `async def`, and decorators spread across lines), and string literals
//! *correctly paired with what binds them* - which is what makes it safe for
//! `kv-cli fix` to rewrite a value in place.

use std::ops::Range;
use tree_sitter::{Node, Parser};

#[derive(Debug, Clone)]
pub struct StringLiteral {
    /// Whole literal, including prefix and quotes.
    pub span: Range<usize>,
    /// The body between the quotes.
    pub content: Range<usize>,
    /// True for an f-string containing at least one `{...}` substitution.
    pub has_interpolation: bool,
}

/// How a string literal came to be bound to a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingSource {
    /// `NAME = "..."` or `NAME: str = "..."`
    Assignment,
    /// `{"name": "..."}`
    DictEntry,
    /// `f(name="...")`
    KeywordArgument,
    /// `(name := "...")`
    Walrus,
    /// `def f(name="...")` - detected, but never rewritten: an `os.environ`
    /// default would be evaluated at definition time, not per call.
    ParameterDefault,
}

/// A string literal bound to a name. Tuple targets are paired element-wise,
/// so `a, b = "x", "y"` yields `a->"x"` and `b->"y"`.
#[derive(Debug, Clone)]
pub struct Binding {
    pub key: String,
    pub value: StringLiteral,
    pub line: usize,
    pub source: BindingSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Normal,
    /// `*args`
    Star,
    /// `**kwargs`
    DoubleStar,
    /// Bare `*` and `/` separators.
    Marker,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub kind: ParamKind,
    pub span: Range<usize>,
    pub has_default: bool,
}

#[derive(Debug, Clone)]
pub struct FunctionDef {
    pub name: String,
    pub line: usize,
    /// Byte offset of `def` (or `async`).
    pub start: usize,
    /// Byte offset of the `(` opening the parameter list.
    pub paren_open: usize,
    /// Byte offset of the matching `)`.
    pub paren_close: usize,
    pub params: Vec<Param>,
    /// Dotted decorator names, outermost first.
    pub decorators: Vec<String>,
}

impl FunctionDef {
    pub fn has_param(&self, name: &str) -> bool {
        self.params
            .iter()
            .any(|p| p.kind != ParamKind::Marker && p.name == name)
    }
}

/// Module-level import facts needed to insert `import os` correctly.
#[derive(Debug, Default)]
pub struct Imports {
    /// True only when `os` itself is bound. `from os import environ` does not
    /// count - it leaves `os.environ` undefined.
    pub binds_os: bool,
    /// Offset of the first real (non-`__future__`) top-level import.
    pub first: Option<usize>,
    /// Offset just past the module prelude: shebang, comments, docstring and
    /// `__future__` imports.
    pub after_prelude: usize,
}

#[derive(Debug)]
pub struct Analysis {
    pub functions: Vec<FunctionDef>,
    pub bindings: Vec<Binding>,
    pub imports: Imports,
    /// True when tree-sitter recovered from a syntax error. Findings are still
    /// reported, but the file is never rewritten.
    pub has_error: bool,
    line_starts: Vec<usize>,
    source_len: usize,
}

impl Analysis {
    /// 1-based line number for a byte offset.
    pub fn line_of(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }

    pub fn line_span(&self, line: usize) -> Range<usize> {
        let start = self.line_starts[line - 1];
        let end = self
            .line_starts
            .get(line)
            .copied()
            .unwrap_or(self.source_len);
        start..end.max(start)
    }
}

fn line_starts(source: &str) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

/// Parse `source` and extract everything the contract engine needs.
pub fn analyze(source: &str) -> Analysis {
    let mut analysis = Analysis {
        functions: Vec::new(),
        bindings: Vec::new(),
        imports: Imports::default(),
        has_error: false,
        line_starts: line_starts(source),
        source_len: source.len(),
    };

    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .is_err()
    {
        // The grammar is compiled in; this cannot fail in a working build.
        analysis.has_error = true;
        return analysis;
    }
    let Some(tree) = parser.parse(source, None) else {
        analysis.has_error = true;
        return analysis;
    };

    analysis.has_error = tree.root_node().has_error();
    let mut ctx = Ctx {
        source,
        line_starts: line_starts(source),
        functions: Vec::new(),
        bindings: Vec::new(),
    };
    let root = tree.root_node();
    walk(root, &mut ctx);
    analysis.imports = scan_imports(root, &ctx);

    ctx.functions.sort_by_key(|f| f.start);
    ctx.bindings.sort_by_key(|b| b.value.span.start);
    analysis.functions = ctx.functions;
    analysis.bindings = ctx.bindings;
    analysis
}

/// Inspect only the module's top level: what is imported, and where a new
/// import may be inserted.
fn scan_imports(root: Node, ctx: &Ctx) -> Imports {
    let mut imports = Imports::default();
    let mut prelude_open = true;
    let mut seen_docstring = false;

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        let kind = child.kind();
        let is_future = kind == "import_from_statement"
            && child
                .child_by_field_name("module_name")
                .map(|n| ctx.text(n))
                == Some("__future__");
        let is_docstring = !seen_docstring
            && kind == "expression_statement"
            && child.named_child(0).map(|n| n.kind()) == Some("string");

        if kind == "import_statement" {
            if binds_os(child, ctx) {
                imports.binds_os = true;
            }
            if imports.first.is_none() {
                imports.first = Some(child.start_byte());
            }
        } else if kind == "import_from_statement" && !is_future && imports.first.is_none() {
            imports.first = Some(child.start_byte());
        }

        if prelude_open {
            if kind == "comment" || is_future || is_docstring {
                seen_docstring |= is_docstring;
                imports.after_prelude = ctx.next_line_start(child.end_byte().saturating_sub(1));
            } else {
                prelude_open = false;
            }
        }
    }
    imports
}

/// True for `import os` / `import os.path`, which bind the name `os`.
/// `import os as o` and `from os import environ` do not.
fn binds_os(node: Node, ctx: &Ctx) -> bool {
    named_children(node).into_iter().any(|child| {
        child.kind() == "dotted_name" && {
            let t = ctx.text(child);
            t == "os" || t.starts_with("os.")
        }
    })
}

struct Ctx<'a> {
    source: &'a str,
    line_starts: Vec<usize>,
    functions: Vec<FunctionDef>,
    bindings: Vec<Binding>,
}

impl Ctx<'_> {
    fn line_of(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }
    fn next_line_start(&self, offset: usize) -> usize {
        self.line_starts
            .get(self.line_of(offset))
            .copied()
            .unwrap_or(self.source.len())
    }
    fn text(&self, node: Node) -> &str {
        self.source.get(node.byte_range()).unwrap_or("")
    }
}

fn walk(node: Node, ctx: &mut Ctx) {
    match node.kind() {
        "function_definition" => {
            if let Some(f) = function_def(node, ctx) {
                ctx.functions.push(f);
            }
        }
        "assignment" => collect_assignment(node, ctx),
        "pair" => bind(
            node.child_by_field_name("key"),
            node.child_by_field_name("value"),
            BindingSource::DictEntry,
            ctx,
        ),
        "keyword_argument" => bind(
            node.child_by_field_name("name"),
            node.child_by_field_name("value"),
            BindingSource::KeywordArgument,
            ctx,
        ),
        "named_expression" => bind(
            node.child_by_field_name("name"),
            node.child_by_field_name("value"),
            BindingSource::Walrus,
            ctx,
        ),
        "default_parameter" | "typed_default_parameter" => bind(
            node.child_by_field_name("name"),
            node.child_by_field_name("value"),
            BindingSource::ParameterDefault,
            ctx,
        ),
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, ctx);
    }
}

fn is_sequence(node: Node) -> bool {
    matches!(
        node.kind(),
        "pattern_list" | "tuple_pattern" | "expression_list" | "tuple"
    )
}

fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    let kids: Vec<Node<'t>> = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() != "comment")
        .collect();
    kids
}

fn collect_assignment(node: Node, ctx: &mut Ctx) {
    let (Some(left), Some(right)) = (
        node.child_by_field_name("left"),
        node.child_by_field_name("right"),
    ) else {
        return;
    };

    // `a, b = "x", "y"` - pair element-wise. Anything that does not line up
    // one-to-one (a starred target, a call on the right) is left alone rather
    // than guessed at.
    if is_sequence(left) && is_sequence(right) {
        let targets = named_children(left);
        let values = named_children(right);
        if targets.len() == values.len() {
            for (t, v) in targets.iter().zip(values.iter()) {
                bind(Some(*t), Some(*v), BindingSource::Assignment, ctx);
            }
        }
        return;
    }

    bind(Some(left), Some(right), BindingSource::Assignment, ctx);
}

fn bind(name: Option<Node>, value: Option<Node>, source: BindingSource, ctx: &mut Ctx) {
    let (Some(name), Some(value)) = (name, value) else {
        return;
    };
    if value.kind() != "string" {
        return;
    }
    let Some(key) = binding_name(name, ctx) else {
        return;
    };
    let Some(literal) = string_literal(value, ctx) else {
        return;
    };
    let line = ctx.line_of(literal.span.start);
    ctx.bindings.push(Binding {
        key,
        value: literal,
        line,
        source,
    });
}

fn binding_name(node: Node, ctx: &Ctx) -> Option<String> {
    match node.kind() {
        // `attribute` covers `self.password` / `cfg.api_key`.
        "identifier" | "attribute" => Some(ctx.text(node).split_whitespace().collect()),
        "string" => string_literal(node, ctx).map(|l| ctx.source[l.content].to_string()),
        _ => None,
    }
}

fn string_literal(node: Node, _ctx: &Ctx) -> Option<StringLiteral> {
    let span = node.byte_range();
    let mut content_start = span.start;
    let mut content_end = span.end;
    let mut has_interpolation = false;

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "string_start" => content_start = child.end_byte(),
            "string_end" => content_end = child.start_byte(),
            "interpolation" => has_interpolation = true,
            _ => {}
        }
    }
    if content_end < content_start {
        content_end = content_start;
    }
    Some(StringLiteral {
        span,
        content: content_start..content_end,
        has_interpolation,
    })
}

fn function_def(node: Node, ctx: &Ctx) -> Option<FunctionDef> {
    let name_node = node.child_by_field_name("name")?;
    let params_node = node.child_by_field_name("parameters")?;
    let start = node.start_byte();

    Some(FunctionDef {
        name: ctx.text(name_node).to_string(),
        line: ctx.line_of(start),
        start,
        paren_open: params_node.start_byte(),
        paren_close: params_node.end_byte().saturating_sub(1),
        params: parse_params(params_node, ctx),
        decorators: decorators_of(node, ctx),
    })
}

fn parse_params(node: Node, ctx: &Ctx) -> Vec<Param> {
    let mut out = Vec::new();
    for child in named_children(node) {
        let (kind, name_node, has_default) = match child.kind() {
            "identifier" => (ParamKind::Normal, Some(child), false),
            "typed_parameter" => (ParamKind::Normal, child.named_child(0), false),
            "default_parameter" | "typed_default_parameter" => {
                (ParamKind::Normal, child.child_by_field_name("name"), true)
            }
            "list_splat_pattern" => (ParamKind::Star, child.named_child(0), false),
            "dictionary_splat_pattern" => (ParamKind::DoubleStar, child.named_child(0), false),
            _ => (ParamKind::Marker, None, false),
        };
        out.push(Param {
            name: name_node
                .map(|n| ctx.text(n).to_string())
                .unwrap_or_default(),
            kind,
            span: child.byte_range(),
            has_default,
        });
    }
    out
}

fn decorators_of(node: Node, ctx: &Ctx) -> Vec<String> {
    let Some(parent) = node.parent() else {
        return Vec::new();
    };
    if parent.kind() != "decorated_definition" {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut cursor = parent.walk();
    for decorator in parent.children(&mut cursor) {
        if decorator.kind() != "decorator" {
            continue;
        }
        let mut inner = decorator.walk();
        for child in decorator.children(&mut inner) {
            if child.kind() == "@" {
                continue;
            }
            // `@task(retries=3)` - name the callee, not the whole call. This
            // works identically whether the call spans one line or ten.
            let target = if child.kind() == "call" {
                child.child_by_field_name("function").unwrap_or(child)
            } else {
                child
            };
            out.push(ctx.text(target).split_whitespace().collect());
            break;
        }
    }
    out
}

/// Shell-style wildcard match supporting `*`, `?` and `**`.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    wc(pattern.as_bytes(), text.as_bytes())
}

fn wc(p: &[u8], t: &[u8]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    match p[0] {
        b'*' => {
            let mut rest = &p[1..];
            while rest.first() == Some(&b'*') {
                rest = &rest[1..];
            }
            if wc(rest, t) {
                return true;
            }
            (0..t.len()).any(|i| wc(rest, &t[i + 1..]))
        }
        b'?' => !t.is_empty() && wc(&p[1..], &t[1..]),
        c => !t.is_empty() && t[0] == c && wc(&p[1..], &t[1..]),
    }
}

/// Match a path pattern against a `/`-separated relative path.
pub fn path_match(pattern: &str, path: &str) -> bool {
    if wildcard_match(pattern, path) {
        return true;
    }
    // `**/` is also allowed to match zero leading segments.
    if let Some(rest) = pattern.strip_prefix("**/") {
        return path_match(rest, path);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(src: &str) -> Vec<(String, String)> {
        let a = analyze(src);
        a.bindings
            .iter()
            .map(|b| (b.key.clone(), src[b.value.content.clone()].to_string()))
            .collect()
    }

    #[test]
    fn comments_and_strings_do_not_confuse_the_parser() {
        let a = analyze("x = \"secret#value\"  # def not_a_function():\ny = 1\n");
        assert!(a.functions.is_empty());
        assert_eq!(keys("x = \"secret#value\"\n")[0].1, "secret#value");
    }

    #[test]
    fn triple_quoted_docstring_is_not_code() {
        let src = "def f():\n    \"\"\"doc\n    def not_a_function():\n    \"\"\"\n    return 1\n";
        let a = analyze(src);
        assert_eq!(a.functions.len(), 1);
        assert_eq!(a.functions[0].name, "f");
    }

    #[test]
    fn parses_multiline_signature_with_defaults() {
        let src = "def deploy(\n    name: str,\n    tags: dict = {\"a\": 1},\n    *args,\n    **kwargs,\n):\n    pass\n";
        let a = analyze(src);
        let f = &a.functions[0];
        let names: Vec<_> = f.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["name", "tags", "args", "kwargs"]);
        assert!(f.has_param("tags"));
        assert!(f.params[1].has_default);
        assert!(!f.params[0].has_default);
        assert_eq!(f.params[3].kind, ParamKind::DoubleStar);
    }

    #[test]
    fn separators_and_lambda_defaults_are_classified() {
        let a = analyze("def f(a, /, b, *, c, d=lambda x, y: x):\n    pass\n");
        let kinds: Vec<_> = a.functions[0].params.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            vec![
                ParamKind::Normal,
                ParamKind::Marker,
                ParamKind::Normal,
                ParamKind::Marker,
                ParamKind::Normal,
                ParamKind::Normal,
            ]
        );
        // The lambda's own parameters must not leak into the signature.
        assert!(!a.functions[0].has_param("x"));
        assert!(a.functions[0].has_param("d"));
    }

    #[test]
    fn detects_async_and_decorators() {
        let src = "@kovallent.task(retries=3)\n@other\nasync def run_job(x):\n    pass\n";
        let a = analyze(src);
        let f = &a.functions[0];
        assert_eq!(f.name, "run_job");
        assert_eq!(f.decorators, vec!["kovallent.task", "other"]);
        assert!(src[f.start..].starts_with("async def"));
    }

    /// Regression: a decorator call spread over several lines used to hide the
    /// function from the contract entirely.
    #[test]
    fn multiline_decorator_is_still_a_decorator() {
        let src = "@kovallent.task(\n    retries=3,\n)\ndef orchestrate(x):\n    pass\n";
        let a = analyze(src);
        assert_eq!(a.functions[0].decorators, vec!["kovallent.task"]);
    }

    /// Regression: element-wise pairing. Previously `db_password` was matched
    /// with `"localhost"`, which made `fix` rewrite the wrong literal.
    #[test]
    fn tuple_unpacking_pairs_element_wise() {
        assert_eq!(
            keys("host, db_password = \"localhost\", \"s3cr3t\"\n"),
            vec![
                ("host".to_string(), "localhost".to_string()),
                ("db_password".to_string(), "s3cr3t".to_string()),
            ]
        );
    }

    #[test]
    fn mismatched_tuple_arity_binds_nothing() {
        assert!(keys("a, b = split(\"x,y\")\n")
            .iter()
            .all(|(k, _)| k != "a" && k != "b"));
    }

    #[test]
    fn binds_every_syntactic_form() {
        assert_eq!(keys("API_KEY: str = \"v1\"\n")[0].0, "API_KEY");
        assert_eq!(keys("cfg = {\"password\": \"v2\"}\n")[0].0, "password");
        assert_eq!(keys("connect(password=\"v3\")\n")[0].0, "password");
        assert_eq!(
            keys("if (api_token := \"v4\"):\n    pass\n")[0].0,
            "api_token"
        );
        assert_eq!(keys("self.secret = \"v5\"\n")[0].0, "self.secret");
    }

    #[test]
    fn comparison_is_not_a_binding() {
        assert!(keys("if password == \"abcd1234\":\n    pass\n").is_empty());
    }

    /// PEP 701: same-type quotes nested inside an f-string (Python 3.12+).
    #[test]
    fn pep701_nested_quotes() {
        let src = "label = f\"env={cfg[\"environment\"]}\"\nPASSWORD = \"abcd1234\"\n";
        let a = analyze(src);
        assert!(!a.has_error);
        // The outer f-string is interpolated; the inner literal is its own node.
        let label = a.bindings.iter().find(|b| b.key == "label").unwrap();
        assert!(label.value.has_interpolation);
        // Analysis after the f-string is unaffected.
        assert!(a.bindings.iter().any(|b| b.key == "PASSWORD"));
    }

    #[test]
    fn fstring_interpolation_flag() {
        let a = analyze("token = f\"{prefix}-value\"\nplain = f\"novars\"\n");
        assert!(a.bindings[0].value.has_interpolation);
        assert!(!a.bindings[1].value.has_interpolation);
    }

    #[test]
    fn nested_and_method_definitions_are_found() {
        let src =
            "class A:\n    def run_m(self, **kw):\n        def inner_x():\n            pass\n";
        let a = analyze(src);
        let names: Vec<_> = a.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["run_m", "inner_x"]);
    }

    #[test]
    fn line_numbers_track_multiline_strings() {
        let src = "a = 1\nb = \"\"\"\n\n\"\"\"\nc = 2\n";
        let a = analyze(src);
        assert_eq!(a.line_of(src.find("c = 2").unwrap()), 5);
    }

    #[test]
    fn syntax_errors_are_reported_not_fatal() {
        let a = analyze("def broken(:\n    pass\n");
        assert!(a.has_error);
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("deploy*", "deploy_app"));
        assert!(wildcard_match("*_pipeline", "etl_pipeline"));
        assert!(!wildcard_match("run_*", "deploy"));
        // Matching is case-sensitive; secret key patterns lower-case both sides.
        assert!(wildcard_match("*password*", "db_password"));
        assert!(!wildcard_match("*password*", "DB_PASSWORD"));
        assert!(path_match("**/*.py", "a.py"));
        assert!(path_match("**/*.py", "pkg/sub/a.py"));
        assert!(path_match("**/.venv/**", ".venv/lib/x.py"));
        assert!(!path_match("**/.venv/**", "src/app.py"));
    }
}
