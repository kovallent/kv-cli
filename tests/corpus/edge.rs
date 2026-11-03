//! Generated edge cases: malformed dbt Python models, Jinja in dbt YAML,
//! deeply nested structures and large files.
//!
//! Every file is generated at test time, so nothing large or token-shaped is
//! committed. Labels follow the corpus rules and are stated here by a person,
//! not derived from kv-cli:
//!
//! - **Credentials are reported wherever they sit**, broken code included.
//!   A credential in a file that does not parse is still in the repository.
//!   Only an unreadable (non-UTF-8) file is exempt, since kv-cli reports the
//!   whole file instead of guessing at its encoding.
//! - **References are never credentials**: `{{ env_var('X') }}`, `${X}` and
//!   broken templates that hold no literal. A literal inside Jinja, such as
//!   `env_var('X', 's3cr3t-…')` or `{{ 's3cr3t-…' }}`, is reported when its
//!   name is a secret name, like a `getenv` fallback in Python.
//! - **Each file is labelled as parseable or not.** A file is a syntax error
//!   exactly when CPython 3.9 rejects it, which was checked case by case. That
//!   becomes a case of its own: `skip:syntax_error` is expected, or must not
//!   appear.
//!
//! Case ids name a probe (`edge:<path>:<probe>:<code>`), not a line, so they
//! survive edits to the generator.

use super::{expand, Case, Truth};

/// A file kv-cli should list in `run.skipped` with this reason.
pub const SKIP_SYNTAX: &str = "skip:syntax_error";
pub const SKIP_UNREADABLE: &str = "skip:unreadable";

/// The literal every name-rule probe uses: real-looking, not a known shape.
const LITERAL: &str = "s3cr3t-value-1234";

pub struct EdgeFile {
    pub path: String,
    pub bytes: Vec<u8>,
    /// Large inputs, timed against the budget as well as labelled.
    pub stress: bool,
}

/// The lines of one file and the labels on them.
struct Doc {
    path: String,
    group: String,
    lines: Vec<String>,
    cases: Vec<Case>,
}

impl Doc {
    fn new(path: impl Into<String>) -> Self {
        let path = path.into();
        Doc {
            group: super::group_of(&path),
            path,
            lines: Vec::new(),
            cases: Vec::new(),
        }
    }

    fn line(&mut self, s: impl Into<String>) {
        self.lines.push(s.into());
    }

    fn label(&mut self, probe: &str, code: &'static str, truth: Truth, s: impl Into<String>) {
        self.lines.push(s.into());
        self.cases.push(Case {
            id: format!("edge:{}:{probe}:{code}", self.path),
            path: self.path.clone(),
            line: self.lines.len(),
            code,
            truth,
            group: self.group.clone(),
        });
    }

    fn report(&mut self, probe: &str, code: &'static str, s: impl Into<String>) {
        self.label(probe, code, Truth::Report, s);
    }

    fn clean(&mut self, probe: &str, code: &'static str, s: impl Into<String>) {
        self.label(probe, code, Truth::Clean, s);
    }

    /// Adds the file-level case, then renders: placeholders expanded, lines
    /// joined with `eol`.
    fn finish(self, out: &mut Generated, skip: Option<&'static str>, eol: &str, stress: bool) {
        let (code, truth) = match skip {
            Some(code) => (code, Truth::Report),
            None => (SKIP_SYNTAX, Truth::Clean),
        };
        let mut cases = self.cases;
        cases.push(Case {
            id: format!("edge:{}:file:{code}", self.path),
            path: self.path.clone(),
            line: 0,
            code,
            truth,
            group: self.group.clone(),
        });
        let text = expand(&(self.lines.join(eol) + eol));
        // A Latin-1 `é`, written as the one byte UTF-8 cannot start with.
        let bytes = match text.split_once("@@LATIN1@@") {
            Some((a, b)) => [a.as_bytes(), &[0xE9], b.as_bytes()].concat(),
            None => text.into_bytes(),
        };
        out.files.push(EdgeFile {
            path: self.path,
            bytes,
            stress,
        });
        out.cases.extend(cases);
    }
}

#[derive(Default)]
pub struct Generated {
    pub files: Vec<EdgeFile>,
    pub cases: Vec<Case>,
}

pub fn generate() -> Generated {
    let mut out = Generated::default();
    dbt_models(&mut out);
    jinja_values(&mut out);
    jinja_structure(&mut out);
    nesting(&mut out);
    stress(&mut out);
    out
}

// --- malformed dbt Python models -------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Effect {
    /// The damage is local; the code after the model reads as code.
    Local,
    /// An unclosed bracket or string runs on, so no function follows it.
    RunsOn,
    /// The file ends inside the damage.
    Truncated,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Special {
    None,
    Bom,
    Crlf,
    Latin1,
}

struct Variant {
    id: &'static str,
    /// Replaces `def model(dbt, session):`.
    def_line: Option<&'static str>,
    decorators: &'static [&'static str],
    /// Inserted into the model body, after `orders = dbt.ref("orders")`.
    body: &'static [&'static str],
    effect: Effect,
    /// Whether CPython 3.9 rejects the file.
    syntax_error: bool,
    special: Special,
}

const fn v(
    id: &'static str,
    body: &'static [&'static str],
    effect: Effect,
    syntax_error: bool,
) -> Variant {
    Variant {
        id,
        def_line: None,
        decorators: &[],
        body,
        effect,
        syntax_error,
        special: Special::None,
    }
}

const VARIANTS: &[Variant] = &[
    v("well-formed", &[], Effect::Local, false),
    // Truncated mid-construct: the file ends inside it.
    v(
        "truncated-call",
        &["    df = orders.filter("],
        Effect::Truncated,
        true,
    ),
    v(
        "truncated-string",
        &["    name = \"unterminated"],
        Effect::Truncated,
        true,
    ),
    v(
        "truncated-dict",
        &["    cfg = {\"a\": 1,"],
        Effect::Truncated,
        true,
    ),
    v(
        "truncated-def",
        &["    def helper(a,"],
        Effect::Truncated,
        true,
    ),
    v(
        "truncated-decorator",
        &["    @dbt."],
        Effect::Truncated,
        true,
    ),
    v(
        "truncated-backslash",
        &["    total = 1 + \\"],
        Effect::Truncated,
        true,
    ),
    // Unclosed brackets and strings that run on into the rest of the file.
    v(
        "unclosed-paren",
        &["    df = orders.filter(F.col(\"a\") > 1"],
        Effect::RunsOn,
        true,
    ),
    v(
        "unclosed-bracket",
        &["    cols = [\"a\", \"b\""],
        Effect::RunsOn,
        true,
    ),
    v(
        "unclosed-brace",
        &["    cfg = {\"a\": 1"],
        Effect::RunsOn,
        true,
    ),
    v(
        "unclosed-docstring",
        &["    \"\"\"docstring never closed"],
        Effect::RunsOn,
        true,
    ),
    v(
        "unclosed-single-docstring",
        &["    '''never closed"],
        Effect::RunsOn,
        true,
    ),
    v(
        "jinja-comment",
        &["    {# legacy filter #}"],
        Effect::RunsOn,
        true,
    ),
    // Local damage.
    v(
        "unclosed-string",
        &["    name = \"abc", "    x = 1"],
        Effect::Local,
        true,
    ),
    Variant {
        def_line: Some("def model(dbt, session)"),
        ..v("missing-colon", &[], Effect::Local, true)
    },
    v(
        "nested-missing-colon",
        &["    def helper(x)", "        return x"],
        Effect::Local,
        true,
    ),
    v(
        "jinja-if-block",
        &[
            "{% if target.name == 'prod' %}",
            "    orders = orders.limit(10)",
            "{% endif %}",
        ],
        Effect::Local,
        true,
    ),
    v(
        "jinja-for-block",
        &[
            "    {% for col in cols %}",
            "    orders = orders.drop(col)",
            "    {% endfor %}",
        ],
        Effect::Local,
        true,
    ),
    v(
        "jinja-set",
        &["    {% set days = 7 %}"],
        Effect::Local,
        true,
    ),
    v("tabs-mixed", &["\tdf = orders"], Effect::Local, true),
    v(
        "unexpected-indent",
        &["        extra = 1"],
        Effect::Local,
        true,
    ),
    v(
        "missing-body",
        &["    def helper():", "    orders = orders.limit(1)"],
        Effect::Local,
        true,
    ),
    v("py2-print", &["    print \"loaded\""], Effect::Local, true),
    v("stray-close", &["    )"], Effect::Local, true),
    v(
        "keyword-param",
        &["    def helper(class):", "        return 1"],
        Effect::Local,
        true,
    ),
    v(
        "missing-comma-kwargs",
        &["    dbt.config(materialized=\"table\" schema=\"x\")"],
        Effect::Local,
        true,
    ),
    v(
        "assign-literal",
        &["    \"orders\" = orders"],
        Effect::Local,
        true,
    ),
    v(
        "stray-except",
        &["    except ValueError:", "        pass"],
        Effect::Local,
        true,
    ),
    v(
        "smart-quotes",
        &["    label = \u{201c}orders\u{201d}"],
        Effect::Local,
        true,
    ),
    v(
        "markdown-fence",
        &["```python", "    x = 1", "```"],
        Effect::Local,
        true,
    ),
    v(
        "ipython-magic",
        &["%pip install pandas"],
        Effect::Local,
        true,
    ),
    v(
        "merge-conflict",
        &[
            "<<<<<<< HEAD",
            "    orders = orders.limit(10)",
            "=======",
            "    orders = orders.limit(20)",
            ">>>>>>> feature",
        ],
        Effect::Local,
        true,
    ),
    v("null-byte", &["    x = 1\0"], Effect::Local, true),
    // Well-formed, but unusual.
    v(
        "jinja-expression",
        &["    df = {{ ref('orders') }}"],
        Effect::Local,
        false,
    ),
    v(
        "jinja-config-call",
        &["    {{ config(materialized='table') }}"],
        Effect::Local,
        false,
    ),
    v(
        "fstring-jinja",
        &["    label = f\"{{ ref('orders') }}\""],
        Effect::Local,
        false,
    ),
    v(
        "long-line",
        &["    blob = \"@@LONG@@\""],
        Effect::Local,
        false,
    ),
    v(
        "long-chain",
        &["    df = orders@@CHAIN@@"],
        Effect::Local,
        false,
    ),
    v(
        "unicode-identifiers",
        &[
            "    donn\u{e9}es = orders",
            "    \u{6570}\u{636e} = donn\u{e9}es",
        ],
        Effect::Local,
        false,
    ),
    v("form-feed", &["\u{c}", "    x = 1"], Effect::Local, false),
    Variant {
        def_line: Some("async def model(dbt, session):"),
        ..v("async-model", &[], Effect::Local, false)
    },
    Variant {
        def_line: Some("def model(dbt: \"dbt\", session: \"Session\") -> \"DataFrame\":"),
        ..v("annotated-model", &[], Effect::Local, false)
    },
    Variant {
        def_line: Some("def model(dbt, session, *args, **kwargs):"),
        ..v("variadic-model", &[], Effect::Local, false)
    },
    Variant {
        decorators: &["@functools.lru_cache(maxsize=None)", "@trace(\"model\")"],
        ..v("decorated-model", &[], Effect::Local, false)
    },
    Variant {
        special: Special::Bom,
        ..v("bom", &[], Effect::Local, false)
    },
    Variant {
        special: Special::Crlf,
        ..v("crlf", &[], Effect::Local, false)
    },
    Variant {
        special: Special::Latin1,
        ..v(
            "latin1",
            &["    # caf@@LATIN1@@ orders"],
            Effect::Local,
            false,
        )
    },
];

fn dbt_models(out: &mut Generated) {
    for var in VARIANTS {
        let mut doc = Doc::new(format!("edge/dbt_models/{}.py", var.id));
        // An unreadable file is reported whole; nothing in it is labelled.
        let labelled = var.special != Special::Latin1;
        let probe = |doc: &mut Doc, probe: &str, code, truth, s: &str| {
            if labelled {
                doc.label(probe, code, truth, s);
            } else {
                doc.line(s);
            }
        };

        doc.line(if var.special == Special::Bom {
            "\u{feff}import os"
        } else {
            "import os"
        });
        doc.line("");
        probe(
            &mut doc,
            "before-name",
            "KV002",
            Truth::Report,
            &format!("API_KEY = \"{LITERAL}\""),
        );
        probe(
            &mut doc,
            "before-decoy",
            "KV002",
            Truth::Clean,
            "secret_name = \"analytics/prod/db\"",
        );
        probe(
            &mut doc,
            "before-jinja",
            "KV002",
            Truth::Clean,
            "DBT_PASSWORD = \"{{ env_var('DBT_PASSWORD') }}\"",
        );
        doc.line("");
        for d in var.decorators {
            doc.line(*d);
        }
        // dbt owns the model signature, so it is never given parameters.
        probe(
            &mut doc,
            "model-def",
            "KV001",
            Truth::Clean,
            var.def_line.unwrap_or("def model(dbt, session):"),
        );
        doc.line("    dbt.config(materialized=\"table\")");
        doc.line("    orders = dbt.ref(\"orders\")");
        for line in var.body {
            doc.line(
                line.replace("@@LONG@@", &"a".repeat(100_000))
                    .replace("@@CHAIN@@", &".filter(x)".repeat(2_000)),
            );
        }
        if var.effect != Effect::Truncated {
            doc.line("    return orders");
            doc.line("");
            if var.effect == Effect::Local {
                probe(
                    &mut doc,
                    "after-kv001",
                    "KV001",
                    Truth::Report,
                    "def run_backfill(days):",
                );
                doc.line("    return days");
                doc.line("");
            }
            probe(
                &mut doc,
                "after-name",
                "KV002",
                Truth::Report,
                &format!("password = \"{LITERAL}\""),
            );
            probe(
                &mut doc,
                "after-token",
                "KV002",
                Truth::Report,
                "banner = \"@@GITHUB_TOKEN@@\"",
            );
            probe(
                &mut doc,
                "after-decoy",
                "KV002",
                Truth::Clean,
                "token_url = \"https://auth.example.com/token\"",
            );
        }

        let skip = match (var.special, var.syntax_error) {
            (Special::Latin1, _) => Some(SKIP_UNREADABLE),
            (_, true) => Some(SKIP_SYNTAX),
            _ => None,
        };
        let eol = if var.special == Special::Crlf {
            "\r\n"
        } else {
            "\n"
        };
        doc.finish(out, skip, eol, false);
    }
}

// --- Jinja in dbt YAML -----------------------------------------------------

#[derive(Clone, Copy)]
enum Class {
    /// A reference a renderer resolves: never a credential.
    Templated,
    /// Holds a real-looking literal: a credential under a secret name.
    Literal,
    /// Holds a credential shape: a credential under any name.
    Token,
    /// A broken template with no literal in it: not a credential.
    Broken,
}

const JINJA_VALUES: &[(&str, &str, Class)] = &[
    ("env-var", "{{ env_var('DBT_PASSWORD') }}", Class::Templated),
    ("env-var-double-quotes", "{{ env_var(\"DBT_PASSWORD\") }}", Class::Templated),
    ("env-var-tight", "{{env_var('DBT_PASSWORD')}}", Class::Templated),
    ("env-var-whitespace-control", "{{- env_var('DBT_PASSWORD') -}}", Class::Templated),
    ("env-var-filter", "{{ env_var('DBT_PASSWORD') | trim }}", Class::Templated),
    ("env-var-empty-default", "{{ env_var('DBT_PASSWORD', '') }}", Class::Templated),
    ("env-var-placeholder-default", "{{ env_var('DBT_PASSWORD', 'changeme') }}", Class::Templated),
    ("env-var-literal-default", "{{ env_var('DBT_PASSWORD', 's3cr3t-value-1234') }}", Class::Literal),
    ("env-var-token-default", "{{ env_var('DBT_PASSWORD', '@@GITHUB_TOKEN@@') }}", Class::Token),
    ("var", "{{ var('db_password') }}", Class::Templated),
    ("target-attribute", "{{ target.password }}", Class::Templated),
    ("embedded", "prefix-{{ env_var('DBT_PASSWORD') }}-suffix", Class::Templated),
    ("two-references", "{{ env_var('DBT_USER') }}:{{ env_var('DBT_PASSWORD') }}", Class::Templated),
    ("jinja-string-literal", "{{ 's3cr3t-value-1234' }}", Class::Literal),
    ("jinja-literal-filter", "{{ 's3cr3t-value-1234' | trim }}", Class::Literal),
    ("if-branch-literal", "{% if target.name == 'prod' %}s3cr3t-value-1234{% else %}{{ env_var('DBT_PASSWORD') }}{% endif %}", Class::Literal),
    ("if-branch-references", "{% if target.name == 'prod' %}{{ env_var('PROD_PW') }}{% else %}{{ env_var('DEV_PW') }}{% endif %}", Class::Templated),
    ("broken-unclosed-call", "{{ env_var('DBT_PASSWORD' }}", Class::Broken),
    ("broken-unclosed-braces", "{{ env_var('DBT_PASSWORD')", Class::Broken),
    ("broken-no-opening", "env_var('DBT_PASSWORD') }}", Class::Broken),
    ("shell-variable", "${DBT_PASSWORD}", Class::Templated),
    ("plain-literal", LITERAL, Class::Literal),
    ("plain-token", "@@GITHUB_TOKEN@@", Class::Token),
];

/// Profile keys, and whether each names a secret.
const PROFILE_KEYS: &[(&str, bool)] = &[
    ("password", true),
    ("token", true),
    ("api_key", true),
    ("private_key_passphrase", true),
    ("user", false),
    ("secret_name", false),
];

fn jinja_truth(class: Class, secret_name: bool) -> Truth {
    match class {
        Class::Token => Truth::Report,
        Class::Literal if secret_name => Truth::Report,
        _ => Truth::Clean,
    }
}

/// The same value as a double-quoted, single-quoted and plain YAML scalar.
fn yaml_quotings(value: &str) -> [(&'static str, String); 3] {
    [
        (
            "double",
            format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"")),
        ),
        ("single", format!("'{}'", value.replace('\'', "''"))),
        ("plain", value.to_string()),
    ]
}

fn jinja_values(out: &mut Generated) {
    for (id, value, class) in JINJA_VALUES {
        for (quoting, scalar) in yaml_quotings(value) {
            let mut doc = Doc::new(format!("edge/jinja/{id}/{quoting}/profiles.yml"));
            doc.line("analytics:");
            doc.line("  target: dev");
            doc.line("  outputs:");
            doc.line("    dev:");
            doc.line("      type: snowflake");
            doc.line("      account: \"{{ env_var('SNOWFLAKE_ACCOUNT') }}\"");
            for (key, secret) in PROFILE_KEYS {
                let truth = jinja_truth(*class, *secret);
                doc.label(key, "KV002", truth, format!("      {key}: {scalar}"));
            }
            doc.line("      threads: 4");
            doc.finish(out, None, "\n", false);
        }
    }
}

/// Jinja statements and comments between YAML lines, as dbt projects use them.
fn jinja_structure(out: &mut Generated) {
    let mut doc = Doc::new("edge/jinja/structure/dbt_project.yml");
    doc.line("name: analytics");
    doc.line("version: \"1.0.0\"");
    doc.report(
        "jinja-set-literal",
        "KV002",
        format!("{{% set db_password = '{LITERAL}' %}}"),
    );
    doc.clean(
        "jinja-set-reference",
        "KV002",
        "{% set db_password = env_var('DBT_PASSWORD') %}",
    );
    doc.line("vars:");
    doc.line("  {% if target.name == 'prod' %}");
    doc.report(
        "if-branch-literal",
        "KV002",
        format!("  api_key: {LITERAL}"),
    );
    doc.line("  {% else %}");
    doc.clean(
        "else-branch-reference",
        "KV002",
        "  api_key: \"{{ env_var('API_KEY') }}\"",
    );
    doc.line("  {% endif %}");
    doc.report(
        "token-after-blocks",
        "KV002",
        "  token: \"@@GITHUB_TOKEN@@\"",
    );
    // Commented out: a name with a literal is not a credential in a comment
    // (as in Python), but a credential shape is still in the repository.
    doc.clean(
        "comment-name-literal",
        "KV002",
        format!("  {{# password: {LITERAL} #}}"),
    );
    doc.report("comment-token", "KV002", "  {# token: @@GITHUB_TOKEN@@ #}");
    doc.line("  {%- for env in ['dev', 'prod'] %}");
    doc.clean(
        "loop-reference",
        "KV002",
        "  password_{{ env }}: \"{{ env_var('PW_' ~ env | upper) }}\"",
    );
    doc.line("  {%- endfor %}");
    doc.report(
        "after-loop-literal",
        "KV002",
        format!("  client_secret: {LITERAL}"),
    );
    doc.line("models:");
    doc.line("  analytics:");
    doc.line("    +schema: \"{{ 'prod' if target.name == 'prod' else 'dev' }}\"");
    doc.finish(out, None, "\n", false);
}

// --- deep nesting ----------------------------------------------------------

const DEEP: &[usize] = &[1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233, 377, 610, 1000];
/// CPython rejects bracket nesting past 199, so Python stops below that.
const DEEP_PY: &[usize] = &[1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144];

/// The two probes placed at the deepest level: (probe, key, value, truth).
const DEEP_PROBES: &[(&str, &str, &str, Truth)] = &[
    ("secret", "password", LITERAL, Truth::Report),
    ("decoy", "secret_name", "analytics/prod/db", Truth::Clean),
];

fn nesting(out: &mut Generated) {
    for &(probe, key, value, truth) in DEEP_PROBES {
        for &d in DEEP {
            // YAML, block style.
            let mut doc = Doc::new(format!("edge/nesting/yaml-block/{probe}-{d}.yml"));
            for i in 0..d {
                doc.line(format!("{}level{i}:", "  ".repeat(i)));
            }
            doc.label(
                probe,
                "KV002",
                truth,
                format!("{}{key}: {value}", "  ".repeat(d)),
            );
            doc.finish(out, None, "\n", false);

            // YAML, flow style, on one line.
            let mut doc = Doc::new(format!("edge/nesting/yaml-flow/{probe}-{d}.yml"));
            let open: String = (0..d).map(|i| format!("{{level{i}: ")).collect();
            doc.label(
                probe,
                "KV002",
                truth,
                format!("config: {open}{{{key}: {value}}}{}", "}".repeat(d)),
            );
            doc.finish(out, None, "\n", false);

            // JSON, objects with an array every third level.
            let mut doc = Doc::new(format!("edge/nesting/json/{probe}-{d}.json"));
            doc.line("{");
            let mut closers = Vec::new();
            for i in 0..d {
                let pad = "  ".repeat(i + 1);
                if i % 3 == 2 {
                    doc.line(format!("{pad}\"level{i}\": [{{"));
                    closers.push(format!("{pad}}}]"));
                } else {
                    doc.line(format!("{pad}\"level{i}\": {{"));
                    closers.push(format!("{pad}}}"));
                }
            }
            doc.label(
                probe,
                "KV002",
                truth,
                format!("{}\"{key}\": \"{value}\"", "  ".repeat(d + 1)),
            );
            for c in closers.into_iter().rev() {
                doc.line(c);
            }
            doc.line("}");
            doc.finish(out, None, "\n", false);
        }

        for &d in DEEP_PY {
            // Python dict literal.
            let mut doc = Doc::new(format!("edge/nesting/py-dict/{probe}-{d}.py"));
            for (n, l) in py_dict(d, key, value).into_iter().enumerate() {
                if n == d + 1 {
                    doc.label(probe, "KV002", truth, l);
                } else {
                    doc.line(l);
                }
            }
            doc.finish(out, None, "\n", false);

            // Python nested calls, the probe a keyword argument.
            let mut doc = Doc::new(format!("edge/nesting/py-call/{probe}-{d}.py"));
            doc.line("pipeline = configure(");
            for i in 0..d {
                doc.line(format!("{}layer{i}(", "    ".repeat(i + 1)));
            }
            doc.label(
                probe,
                "KV002",
                truth,
                format!("{}{key}=\"{value}\",", "    ".repeat(d + 1)),
            );
            for i in (0..d).rev() {
                doc.line(format!("{}),", "    ".repeat(i + 1)));
            }
            doc.line(")");
            doc.finish(out, None, "\n", false);

            // The dict literal again, as a notebook code cell.
            let mut doc = Doc::new(format!("edge/nesting/notebook/{probe}-{d}.ipynb"));
            notebook_cell(&mut doc, &py_dict(d, key, value), d + 1, probe, truth);
            doc.finish(out, None, "\n", false);
        }
    }
}

/// `CONFIG = {` nested `d` deep, the probe on the deepest line (index
/// `d + 1`).
fn py_dict(d: usize, key: &str, value: &str) -> Vec<String> {
    let mut lines = vec!["CONFIG = {".to_string()];
    for i in 0..d {
        lines.push(format!("{}\"level{i}\": {{", "    ".repeat(i + 1)));
    }
    lines.push(format!("{}\"{key}\": \"{value}\",", "    ".repeat(d + 1)));
    for i in (0..d).rev() {
        lines.push(format!("{}}},", "    ".repeat(i + 1)));
    }
    lines.push("}".to_string());
    lines
}

/// One code cell holding `source`, one JSON string per source line; the
/// string for `source[at]` carries the label.
fn notebook_cell(doc: &mut Doc, source: &[String], at: usize, probe: &str, truth: Truth) {
    doc.line("{");
    doc.line(" \"cells\": [");
    doc.line("  {");
    doc.line("   \"cell_type\": \"code\",");
    doc.line("   \"execution_count\": null,");
    doc.line("   \"metadata\": {},");
    doc.line("   \"outputs\": [],");
    doc.line("   \"source\": [");
    for (n, l) in source.iter().enumerate() {
        let comma = if n + 1 < source.len() { "," } else { "" };
        let nl = if n + 1 < source.len() { "\\n" } else { "" };
        let json = format!("    \"{}{nl}\"{comma}", l.replace('"', "\\\""));
        if n == at {
            doc.label(probe, "KV002", truth, json);
        } else {
            doc.line(json);
        }
    }
    doc.line("   ]");
    doc.line("  }");
    doc.line(" ],");
    doc.line(" \"metadata\": {},");
    doc.line(" \"nbformat\": 4,");
    doc.line(" \"nbformat_minor\": 5");
    doc.line("}");
}

// --- large files -----------------------------------------------------------

fn stress(out: &mut Generated) {
    // A dbt Python model with thousands of helpers.
    let mut doc = Doc::new("edge/stress/big_model.py");
    doc.line("import os");
    doc.line("");
    doc.clean("model-def", "KV001", "def model(dbt, session):");
    doc.line("    return dbt.ref(\"orders\")");
    for i in 0..5_000 {
        doc.line("");
        doc.line(format!("def _transform_{i}(df, col=\"c{i}\"):"));
        doc.line(format!("    \"\"\"Step {i}.\"\"\""));
        doc.line(format!("    return df.with_column(col, df[col] * {i})"));
    }
    doc.line("");
    doc.report("kv001", "KV001", "def run_backfill(days):");
    doc.line("    return days");
    doc.report("name", "KV002", format!("password = \"{LITERAL}\""));
    doc.clean(
        "decoy",
        "KV002",
        "token_url = \"https://auth.example.com/token\"",
    );
    doc.finish(out, None, "\n", true);

    // A dbt schema.yml describing thousands of models.
    let mut doc = Doc::new("edge/stress/schema.yml");
    doc.line("version: 2");
    doc.line("models:");
    for i in 0..3_000 {
        doc.line(format!("  - name: model_{i}"));
        doc.line(format!("    description: \"{{{{ doc('model_{i}') }}}}\""));
        doc.line("    columns:");
        for c in 0..4 {
            doc.line(format!("      - name: col_{c}"));
            doc.line("        tests: [not_null, unique]");
        }
    }
    doc.line("  - name: model_with_meta");
    doc.line("    meta:");
    doc.report("name", "KV002", format!("      api_key: {LITERAL}"));
    doc.clean("decoy", "KV002", "      secret_name: analytics/prod/db");
    doc.finish(out, None, "\n", true);

    // dbt's target/manifest.json is one minified line, often megabytes.
    let mut doc = Doc::new("edge/stress/manifest.json");
    let mut manifest = String::from("{\"nodes\": {");
    for i in 0..15_000 {
        manifest.push_str(&format!(
            "\"model.analytics.m{i}\": {{\"name\": \"m{i}\", \"config\": {{\"materialized\": \"table\", \"tags\": [\"a\", \"b\"]}}, \"description\": \"{}\"}}, ",
            "x".repeat(200)
        ));
    }
    manifest.push_str("\"last\": {\"banner\": \"@@GITHUB_TOKEN@@\"}}}");
    doc.report("token", "KV002", manifest);
    doc.finish(out, None, "\n", true);

    // A single 1 MB line, then code after it.
    let mut doc = Doc::new("edge/stress/long_line.py");
    doc.line(format!("BLOB = \"{}\"", "a".repeat(1_000_000)));
    doc.report("name", "KV002", format!("password = \"{LITERAL}\""));
    doc.finish(out, None, "\n", true);

    // Thousands of Jinja references, then one literal.
    let mut doc = Doc::new("edge/stress/profiles.yml");
    doc.line("analytics:");
    doc.line("  outputs:");
    for i in 0..5_000 {
        doc.line(format!("    target_{i}:"));
        doc.line("      type: snowflake");
        if i == 0 || i == 4_999 {
            doc.clean(
                &format!("reference-{i}"),
                "KV002",
                "      password: \"{{ env_var('DBT_PASSWORD') }}\"",
            );
        } else {
            doc.line("      password: \"{{ env_var('DBT_PASSWORD') }}\"");
        }
    }
    doc.line("    last:");
    doc.report("name", "KV002", format!("      password: {LITERAL}"));
    doc.finish(out, None, "\n", true);

    // A notebook with hundreds of cells; the last output printed a token.
    let mut doc = Doc::new("edge/stress/notebook.ipynb");
    doc.line("{");
    doc.line(" \"cells\": [");
    for i in 0..300 {
        doc.line("  {");
        doc.line("   \"cell_type\": \"code\",");
        doc.line("   \"execution_count\": 1,");
        doc.line("   \"metadata\": {},");
        doc.line(
            "   \"outputs\": [{\"name\": \"stdout\", \"output_type\": \"stream\", \"text\": [",
        );
        for j in 0..100 {
            doc.line(format!("     \"row {j} of cell {i}\\n\","));
        }
        if i == 299 {
            doc.report(
                "output-token",
                "KV002",
                "     \"banner @@GITHUB_TOKEN@@\\n\"",
            );
        } else {
            doc.line("     \"end\\n\"");
        }
        doc.line("   ]}],");
        doc.line(format!("   \"source\": [\"df_{i} = load({i})\"]"));
        doc.line(if i == 299 { "  }" } else { "  }," });
    }
    doc.line(" ],");
    doc.line(" \"metadata\": {},");
    doc.line(" \"nbformat\": 4,");
    doc.line(" \"nbformat_minor\": 5");
    doc.line("}");
    doc.finish(out, None, "\n", true);

    // Nesting far past anything hand-written.
    let mut doc = Doc::new("edge/stress/deep.json");
    let d = 10_000;
    doc.report(
        "token",
        "KV002",
        format!(
            "{}\"banner\": \"@@GITHUB_TOKEN@@\"{}",
            "{\"a\": ".repeat(d) + "{",
            "}".repeat(d + 1)
        ),
    );
    doc.finish(out, None, "\n", true);

    let mut doc = Doc::new("edge/stress/deep.yml");
    let d = 2_000;
    for i in 0..d {
        doc.line(format!("{}l{i}:", " ".repeat(i)));
    }
    doc.report(
        "name",
        "KV002",
        format!("{}password: {LITERAL}", " ".repeat(d)),
    );
    doc.finish(out, None, "\n", true);
}
