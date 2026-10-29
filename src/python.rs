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
    /// The body of each part: one for a plain literal, several for an
    /// implicit concatenation.
    pub parts: Vec<Range<usize>>,
}

impl StringLiteral {
    /// The literal's text with the parts joined - what Python evaluates
    /// `"chan" "geme"` to, before any escape processing.
    pub fn text(&self, source: &str) -> String {
        self.parts.iter().map(|p| &source[p.clone()]).collect()
    }
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
    /// `cfg["name"] = "..."` - keyed by the subscript string.
    Subscript,
    /// `os.environ["NAME"] = "..."` - detected, but never rewritten: replacing
    /// the value with `os.environ["NAME"]` would read the variable it is
    /// meant to set.
    EnvironAssignment,
    /// `os.getenv("NAME", "...")` / `os.environ.get("NAME", "...")` - the
    /// fallback is bound to `NAME`. Detected, but never rewritten: dropping
    /// the fallback changes what the lookup returns when the variable is unset.
    EnvFallback,
    /// `NAME += "..."` - detected, but never rewritten: replacing the literal
    /// would append the whole environment value to whatever came before.
    AugmentedAssignment,
    /// `password == "..."` or `"..." != self.token` - a literal the code
    /// checks a credential against is as hardcoded as one it assigns.
    Comparison,
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
    /// Bare `*`: every parameter after it is keyword-only.
    KeywordSeparator,
    /// Bare `/` separator, or anything else that names no parameter.
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
        self.params.iter().any(|p| {
            !matches!(p.kind, ParamKind::Marker | ParamKind::KeywordSeparator) && p.name == name
        })
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
    /// Root module name of every import, e.g. `snowflake` for
    /// `from snowflake.snowpark import Session`. Used to detect frameworks.
    pub modules: Vec<String>,
}

#[derive(Debug)]
pub struct Analysis {
    pub functions: Vec<FunctionDef>,
    pub bindings: Vec<Binding>,
    pub imports: Imports,
    /// True when tree-sitter recovered from a syntax error. Findings are still
    /// reported, but the file is never rewritten.
    pub has_error: bool,
    /// Byte ranges of every `#` comment, in source order. Ignore markers only
    /// count inside one - a marker in a string literal is data, not intent.
    pub comments: Vec<Range<usize>>,
    /// Every implicit concatenation (`"ghp_abc" "def"`), wherever it appears.
    /// Its joined text is a value no raw-text match can see.
    pub concatenations: Vec<StringLiteral>,
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

    /// True when a comment on `line` contains `marker`.
    pub fn comment_on_line_contains(&self, source: &str, line: usize, marker: &str) -> bool {
        let span = self.line_span(line);
        // Comments are sorted and never span lines: find the first one that
        // ends after the line starts, then walk while they start on it.
        let first = self.comments.partition_point(|c| c.end <= span.start);
        self.comments[first..]
            .iter()
            .take_while(|c| c.start < span.end)
            .any(|c| source[c.clone()].contains(marker))
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
        comments: Vec::new(),
        concatenations: Vec::new(),
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
        line_starts: &analysis.line_starts,
        functions: Vec::new(),
        bindings: Vec::new(),
        comments: Vec::new(),
        concatenations: Vec::new(),
    };
    let root = tree.root_node();
    walk(root, &mut ctx);
    let imports = scan_imports(root, &ctx);

    let Ctx {
        mut functions,
        mut bindings,
        mut comments,
        concatenations,
        ..
    } = ctx;
    functions.sort_by_key(|f| f.start);
    bindings.sort_by_key(|b| b.value.span.start);
    comments.sort_by_key(|c| c.start);
    analysis.imports = imports;
    analysis.functions = functions;
    analysis.bindings = bindings;
    analysis.comments = comments;
    analysis.concatenations = concatenations;
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
            collect_modules(child, ctx, &mut imports.modules);
        } else if kind == "import_from_statement" {
            if !is_future && imports.first.is_none() {
                imports.first = Some(child.start_byte());
            }
            if let Some(m) = child.child_by_field_name("module_name") {
                push_root_module(ctx.text(m), &mut imports.modules);
            }
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

/// Record the root module of every name in an `import a.b, c as d` statement.
fn collect_modules(node: Node, ctx: &Ctx, out: &mut Vec<String>) {
    for child in named_children(node) {
        match child.kind() {
            "dotted_name" => push_root_module(ctx.text(child), out),
            // `import polars as pl` - the module is the aliased name's target.
            "aliased_import" => {
                if let Some(name) = child.child_by_field_name("name") {
                    push_root_module(ctx.text(name), out);
                }
            }
            _ => {}
        }
    }
}

fn push_root_module(dotted: &str, out: &mut Vec<String>) {
    let root = dotted.split('.').next().unwrap_or("").trim();
    if !root.is_empty() && !out.iter().any(|m| m == root) {
        out.push(root.to_string());
    }
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
    line_starts: &'a [usize],
    functions: Vec<FunctionDef>,
    bindings: Vec<Binding>,
    comments: Vec<Range<usize>>,
    concatenations: Vec<StringLiteral>,
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

/// Visit every node in pre-order.
///
/// Iterative on purpose. Tree depth is controlled by whoever wrote the file -
/// a chained `a + b + ...` nests one level per operand - and a recursive walk
/// overflowed the stack on such input, aborting the whole run instead of
/// reporting on it.
fn walk(root: Node, ctx: &mut Ctx) {
    let mut cursor = root.walk();
    loop {
        visit(cursor.node(), ctx);
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

fn visit(node: Node, ctx: &mut Ctx) {
    match node.kind() {
        "comment" => ctx.comments.push(node.byte_range()),
        "function_definition" => {
            if let Some(f) = function_def(node, ctx) {
                ctx.functions.push(f);
            }
        }
        "assignment" => collect_assignment(node, ctx),
        "augmented_assignment" => bind(
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
            BindingSource::AugmentedAssignment,
            ctx,
        ),
        "call" => collect_env_fallback(node, ctx),
        "comparison_operator" => collect_comparison(node, ctx),
        "concatenated_string" => {
            if let Some(literal) = literal_of(node, ctx) {
                ctx.concatenations.push(literal);
            }
        }
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
                bind(Some(*t), Some(*v), assignment_source(*t, ctx), ctx);
            }
        }
        return;
    }

    bind(Some(left), Some(right), assignment_source(left, ctx), ctx);
}

/// How an assignment target binds: plainly, through a subscript, or into
/// the process environment.
fn assignment_source(target: Node, ctx: &Ctx) -> BindingSource {
    if target.kind() != "subscript" {
        return BindingSource::Assignment;
    }
    let container: String = target
        .child_by_field_name("value")
        .map(|v| ctx.text(v).split_whitespace().collect())
        .unwrap_or_default();
    if container == "os.environ" || container == "environ" {
        BindingSource::EnvironAssignment
    } else {
        BindingSource::Subscript
    }
}

/// `os.getenv("NAME", "fallback")` and `os.environ.get("NAME", "fallback")`,
/// positional or `default=`. The fallback is a value written into the source
/// under the variable's name - exactly what the lookup was meant to replace -
/// so it is bound to `NAME` and judged by the same key rules.
fn collect_env_fallback(node: Node, ctx: &mut Ctx) {
    let Some(function) = node.child_by_field_name("function") else {
        return;
    };
    let name: String = ctx.text(function).split_whitespace().collect();
    if !matches!(
        name.as_str(),
        "os.getenv" | "getenv" | "os.environ.get" | "environ.get"
    ) {
        return;
    }
    let Some(args) = node.child_by_field_name("arguments") else {
        return;
    };
    let args = named_children(args);
    let Some(var) = args.first().copied().filter(|a| a.kind() == "string") else {
        return;
    };
    let fallback = args
        .get(1)
        .copied()
        .filter(|a| a.kind() != "keyword_argument")
        .or_else(|| {
            args.iter()
                .find(|a| {
                    a.kind() == "keyword_argument"
                        && a.child_by_field_name("name").map(|n| ctx.text(n)) == Some("default")
                })
                .and_then(|a| a.child_by_field_name("value"))
        });
    bind(Some(var), fallback, BindingSource::EnvFallback, ctx);
}

/// `password == "literal"` or `"literal" != self.token`: whichever side is the
/// literal binds to the other. Only a plain two-operand `==` / `!=` - a
/// chained comparison (`a == b == "x"`) is left alone rather than guessed at.
fn collect_comparison(node: Node, ctx: &mut Ctx) {
    let operands = named_children(node);
    let mut cursor = node.walk();
    let operators: Vec<&str> = node
        .children(&mut cursor)
        .filter(|c| !c.is_named())
        .map(|c| c.kind())
        .collect();
    let [left, right] = operands.as_slice() else {
        return;
    };
    if !matches!(operators.as_slice(), ["=="] | ["!="]) {
        return;
    }
    let (name, value) = if literal_of(*right, ctx).is_some() {
        (*left, *right)
    } else {
        (*right, *left)
    };
    bind(Some(name), Some(value), BindingSource::Comparison, ctx);
}

fn bind(name: Option<Node>, value: Option<Node>, source: BindingSource, ctx: &mut Ctx) {
    let (Some(name), Some(value)) = (name, value) else {
        return;
    };
    let Some(literal) = literal_of(value, ctx) else {
        return;
    };
    let Some(key) = binding_name(name, ctx) else {
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

/// The string literal an expression is, looking through parentheses and
/// implicit concatenation: `("s3cr3t" "-pw")` is as hardcoded as `"s3cr3t-pw"`.
///
/// A concatenation's `content` runs from the first part's body to the last
/// part's, quotes between parts included; judge its value by
/// [`StringLiteral::text`], which joins the parts. `span` (what `fix`
/// replaces) is exact either way.
fn literal_of(node: Node, ctx: &Ctx) -> Option<StringLiteral> {
    let mut node = node;
    while node.kind() == "parenthesized_expression" {
        match named_children(node).as_slice() {
            [inner] => node = *inner,
            _ => return None,
        }
    }
    match node.kind() {
        "string" => string_literal(node, ctx),
        "concatenated_string" => {
            let parts: Vec<StringLiteral> = named_children(node)
                .into_iter()
                .filter(|p| p.kind() == "string")
                .filter_map(|p| string_literal(p, ctx))
                .collect();
            let (first, last) = (parts.first()?, parts.last()?);
            Some(StringLiteral {
                span: node.byte_range(),
                content: first.content.start..last.content.end,
                has_interpolation: parts.iter().any(|p| p.has_interpolation),
                parts: parts.iter().map(|p| p.content.clone()).collect(),
            })
        }
        _ => None,
    }
}

fn binding_name(node: Node, ctx: &Ctx) -> Option<String> {
    match node.kind() {
        // `attribute` covers `self.password` / `cfg.api_key`.
        "identifier" | "attribute" => Some(ctx.text(node).split_whitespace().collect()),
        "string" => string_literal(node, ctx).map(|l| ctx.source[l.content].to_string()),
        // `cfg["api_key"]` and `os.environ["API_KEY"]` are keyed by the
        // string; a computed index names nothing we can judge.
        "subscript" => node
            .child_by_field_name("subscript")
            .filter(|s| s.kind() == "string")
            .and_then(|s| binding_name(s, ctx)),
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
        parts: std::iter::once(content_start..content_end).collect(),
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
            "keyword_separator" => (ParamKind::KeywordSeparator, None, false),
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
    // Greedy match with one backtrack point - the most recent `*` - which is
    // complete for `*`/`?` globs and O(len(pattern) * len(text)) at worst.
    // The recursive matcher it replaces backtracked across every `*` at once:
    // a 200 KB dict key against `*sasl*password*` took 33 seconds, and the
    // text is whatever a scanned file contains.
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0, 0);
    // (pattern index just past the last `*`, text index that `*` matched to)
    let mut resume: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some(b'*') => {
                pi += 1;
                resume = Some((pi, ti));
            }
            Some(&c) if c == b'?' || c == t[ti] => {
                pi += 1;
                ti += 1;
            }
            _ => match resume {
                // Let the last `*` absorb one more byte and retry.
                Some((after_star, matched_to)) => {
                    pi = after_star;
                    ti = matched_to + 1;
                    resume = Some((after_star, ti));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

/// Match a `scan.include` / `scan.exclude` pattern against a `/`-separated
/// path relative to the project, with `.gitignore` semantics:
///
/// - `*` and `?` match within one path segment - `src/*.py` is `src/a.py`,
///   not `src/deep/b.py`;
/// - `**` as a whole segment matches any number of segments, including none;
/// - a pattern with no `/` matches the file name at any depth - `*.py` is
///   every Python file, as it is in `.gitignore`.
///
/// `*` used to cross `/`, so `src/*.py` silently reached every subdirectory.
pub fn path_match(pattern: &str, path: &str) -> bool {
    let path: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let pattern = pattern.trim_start_matches('/');
    if !pattern.contains('/') {
        return path
            .last()
            .is_some_and(|name| pattern == "**" || wildcard_match(pattern, name));
    }
    let segments: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&segments, &path)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|i| match_segments(rest, &path[i..])),
        Some((segment, rest)) => {
            path.first().is_some_and(|p| wildcard_match(segment, p))
                && match_segments(rest, &path[1..])
        }
    }
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
                ParamKind::KeywordSeparator,
                ParamKind::Normal,
                ParamKind::Normal,
            ]
        );
        // Neither separator is a parameter name.
        assert!(!a.functions[0].has_param(""));
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
    fn comparisons_bind_whichever_side_is_the_literal() {
        let a = analyze("if password == \"v1\":\n    pass\nif \"v2\" != self.token:\n    pass\n");
        let got: Vec<_> = a
            .bindings
            .iter()
            .map(|b| (b.key.as_str(), b.source))
            .collect();
        assert_eq!(
            got,
            vec![
                ("password", BindingSource::Comparison),
                ("self.token", BindingSource::Comparison),
            ]
        );
        // Ordering, membership and chains are not equality checks on a value.
        assert!(analyze("if password < \"v\":\n    pass\n")
            .bindings
            .is_empty());
        assert!(analyze("if password in (\"a\", \"b\"):\n    pass\n")
            .bindings
            .is_empty());
        assert!(analyze("f(a == password == \"v\")\n").bindings.is_empty());
    }

    #[test]
    fn concatenations_are_collected_wherever_they_appear() {
        let a = analyze("call(\"ghp_abc\" \"def\")\nx = 1\n");
        assert_eq!(a.concatenations.len(), 1);
        assert_eq!(
            a.concatenations[0].text("call(\"ghp_abc\" \"def\")"),
            "ghp_abcdef"
        );
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

    /// The recursive matcher `wildcard_match` replaced, kept as an oracle.
    fn reference_match(p: &[u8], t: &[u8]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        match p[0] {
            b'*' => (0..=t.len()).any(|i| reference_match(&p[1..], &t[i..])),
            b'?' => !t.is_empty() && reference_match(&p[1..], &t[1..]),
            c => !t.is_empty() && t[0] == c && reference_match(&p[1..], &t[1..]),
        }
    }

    /// Exhaustive-ish agreement with the old semantics over a small alphabet,
    /// where backtracking cases are dense.
    #[test]
    fn wildcard_matcher_agrees_with_the_reference() {
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let alphabet = *b"ab*?";
        for _ in 0..20_000 {
            let p: Vec<u8> = (0..next(7)).map(|_| alphabet[next(4) as usize]).collect();
            let t: Vec<u8> = (0..next(9)).map(|_| alphabet[next(2) as usize]).collect();
            let (ps, ts) = (
                std::str::from_utf8(&p).unwrap(),
                std::str::from_utf8(&t).unwrap(),
            );
            assert_eq!(
                wildcard_match(ps, ts),
                reference_match(&p, &t),
                "pattern {ps:?} text {ts:?}"
            );
        }
    }

    /// Regression: 33 seconds on this input with the recursive matcher.
    #[test]
    fn wildcard_matching_is_not_superlinear_on_hostile_text() {
        let text = "sasl".repeat(50_000);
        let start = std::time::Instant::now();
        assert!(!wildcard_match("*sasl*password*", &text));
        assert!(!wildcard_match("*a*a*a*a*a*b", &"a".repeat(100_000)));
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "took {:?}",
            start.elapsed()
        );
    }

    /// Regression: the recursive walk overflowed the stack on deep nesting
    /// and aborted the process. Test threads have a small stack, so this
    /// fails loudly if recursion ever comes back.
    #[test]
    fn deeply_nested_expressions_do_not_overflow_the_stack() {
        let src = format!(
            "x = {}\nDB_PASSWORD = \"s3cr3t-prod-pw\"\n",
            vec!["1"; 100_000].join(" + ")
        );
        let a = analyze(&src);
        assert!(a.bindings.iter().any(|b| b.key == "DB_PASSWORD"));
    }

    #[test]
    fn binds_through_parentheses_and_implicit_concatenation() {
        let src = "A = (\"s3cr3t\" \"-prod\")\nB = ((\"x1234\"))\n";
        let a = analyze(src);
        let a_bind = a.bindings.iter().find(|b| b.key == "A").unwrap();
        // `fix` replaces the whole concatenation, not one part.
        assert_eq!(&src[a_bind.value.span.clone()], "\"s3cr3t\" \"-prod\"");
        assert!(a.bindings.iter().any(|b| b.key == "B"));
        // An interpolated part makes the whole value computed.
        let a = analyze("C = \"x\" f\"{y}\"\n");
        assert!(a.bindings[0].value.has_interpolation);
    }

    #[test]
    fn subscript_targets_bind_by_their_string_key() {
        let a = analyze(
            "cfg[\"db_password\"] = \"v1\"\nos.environ[\"API_KEY\"] = \"v2\"\ncfg[i] = \"v3\"\n",
        );
        let got: Vec<_> = a
            .bindings
            .iter()
            .map(|b| (b.key.as_str(), b.source))
            .collect();
        assert_eq!(
            got,
            vec![
                ("db_password", BindingSource::Subscript),
                ("API_KEY", BindingSource::EnvironAssignment),
            ]
        );
    }

    #[test]
    fn env_lookup_fallbacks_bind_to_the_variable_name() {
        for src in [
            "p = os.getenv(\"DB_PASSWORD\", \"v\")\n",
            "p = getenv(\"DB_PASSWORD\", \"v\")\n",
            "p = os.environ.get(\"DB_PASSWORD\", \"v\")\n",
            "p = os.environ.get(\"DB_PASSWORD\", default=\"v\")\n",
        ] {
            let a = analyze(src);
            assert!(
                a.bindings
                    .iter()
                    .any(|b| b.key == "DB_PASSWORD" && b.source == BindingSource::EnvFallback),
                "{src}"
            );
        }
        // No fallback, nothing bound to the variable.
        let a = analyze("p = os.getenv(\"DB_PASSWORD\")\n");
        assert!(a.bindings.is_empty());
    }

    #[test]
    fn augmented_assignment_binds() {
        let a = analyze("DB_PASSWORD = \"\"\nDB_PASSWORD += \"s3cr3t\"\n");
        let b = a.bindings.iter().find(|b| b.line == 2).unwrap();
        assert_eq!(
            (b.key.as_str(), b.source),
            ("DB_PASSWORD", BindingSource::AugmentedAssignment)
        );
    }

    #[test]
    fn comments_are_recorded_and_strings_are_not_comments() {
        let src = "x = \"# not a comment\"  # real\ny = 1\n";
        let a = analyze(src);
        assert_eq!(a.comments.len(), 1);
        assert!(a.comment_on_line_contains(src, 1, "real"));
        assert!(!a.comment_on_line_contains(src, 1, "not a comment"));
        assert!(!a.comment_on_line_contains(src, 2, "real"));
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

    /// `.gitignore` semantics, one row per rule.
    #[test]
    fn path_patterns_follow_gitignore_rules() {
        for (pattern, path, want) in [
            // `*` stays within a segment.
            ("src/*.py", "src/a.py", true),
            ("src/*.py", "src/deep/b.py", false),
            // `**` crosses any number of segments, including none.
            ("src/**/*.py", "src/a.py", true),
            ("src/**/*.py", "src/deep/er/b.py", true),
            ("src/**", "src/deep/b.py", true),
            ("**/tests/**", "a/tests/b/c.py", true),
            ("**/tests/**", "tests/c.py", true),
            // No `/`: the file name, at any depth.
            ("*.py", "a.py", true),
            ("*.py", "deep/er/a.py", true),
            (".env", "conf/.env", true),
            ("deploy.py", "jobs/deploy.py", true),
            ("*.py", "a.pyc", false),
            // Anchored at the project root; a leading `/` changes nothing.
            ("/src/*.py", "src/a.py", true),
            ("src/*.py", "lib/src/a.py", false),
            // `?` is one character, never a separator.
            ("src/?.py", "src/a.py", true),
            ("src?a.py", "src/a.py", false),
        ] {
            assert_eq!(path_match(pattern, path), want, "{pattern} vs {path}");
        }
    }
}
