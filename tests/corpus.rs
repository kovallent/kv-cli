//! The labelled corpus: what kv-cli *should* report, measured against what it
//! does.
//!
//! Two sources of cases, both labelled by a person rather than derived from
//! the implementation:
//!
//! - `tests/corpus/cases/**` - hand-written files. A line that should be
//!   reported carries `expect: KV00n` in a comment; every other line should
//!   not be. JSON and notebooks have no comments, so `<file>.expect` lists
//!   `LINE CODE` pairs instead. A file that should land in the payload's
//!   `skipped` list says `expect-skip: <reason>`; any other skip fails.
//! - `tests/corpus/matrix/{names,values}.tsv` - labelled key names and values,
//!   expanded across every syntactic form in [`FORMS`]. A case should be
//!   reported when its value is a credential shape, or when a secret name holds
//!   a real-looking literal. See [`truth`].
//!
//! Where kv-cli disagrees with a label, the case is listed in
//! `tests/corpus/baseline.tsv` as a known false positive or miss. The test
//! fails when that list changes in either direction - a regression, or a fix
//! that has not been recorded - so every behaviour change is a visible diff.
//! `KV_CORPUS_BLESS=1 cargo test --test corpus` rewrites the baseline; do it
//! deliberately, and read the diff. `make corpus` prints the precision and
//! recall report.
//!
//! Token-shaped values are never committed: cases say `@@GITHUB_TOKEN@@` and
//! the harness writes a fake of the right shape into a temporary copy, so the
//! repository itself never trips a secret scanner.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus");

/// A deterministic fake of `len` characters drawn from `alphabet`.
fn fill(alphabet: &str, len: usize) -> String {
    alphabet.chars().cycle().take(len).collect()
}

/// Placeholder -> fake credential of the shape the detector expects.
fn fakes() -> Vec<(&'static str, String)> {
    let alnum = "R7c2Kq9Lm4Xv8Bn1Zt6Wy3Hs5Jd0Pf";
    let upper = "R7C2KQ9LM4XV8BN1ZT6WY3HS5JD0PF";
    let hex = "7c2a9f4e8b1d6a3c5e0f";
    vec![
        ("@@GITHUB_TOKEN@@", format!("ghp_{}", fill(alnum, 36))),
        ("@@GITHUB_PAT@@", format!("github_pat_{}", fill(alnum, 40))),
        ("@@AWS_KEY@@", format!("AKIA{}", fill(upper, 16))),
        ("@@SLACK_TOKEN@@", format!("xoxb-{}", fill(alnum, 24))),
        ("@@ANTHROPIC_KEY@@", format!("sk-ant-{}", fill(alnum, 40))),
        ("@@OPENAI_KEY@@", format!("sk-{}", fill(alnum, 48))),
        (
            "@@OPENAI_PROJECT_KEY@@",
            format!("sk-proj-{}", fill(alnum, 40)),
        ),
        ("@@GOOGLE_API_KEY@@", format!("AIza{}", fill(alnum, 35))),
        (
            "@@STRIPE_LIVE_KEY@@",
            format!("sk_live_{}", fill(alnum, 28)),
        ),
        (
            "@@JWT@@",
            format!(
                "eyJ{}.eyJ{}.{}",
                fill(alnum, 16),
                fill(alnum, 24),
                fill(alnum, 20)
            ),
        ),
        ("@@DATABRICKS_PAT@@", format!("dapi{}", fill(hex, 32))),
        (
            "@@PRIVATE_KEY_BLOCK@@",
            "-----BEGIN RSA PRIVATE KEY-----".to_string(),
        ),
    ]
}

fn expand(content: &str) -> String {
    let mut out = content.to_string();
    for (placeholder, fake) in fakes() {
        out = out.replace(placeholder, &fake);
    }
    out
}

/// Labelled outcome for one case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Truth {
    Report,
    Clean,
}

/// One case: an id stable across edits elsewhere, where it lands in the
/// materialized tree, what a person says should happen, and its group for the
/// report.
struct Case {
    id: String,
    path: String,
    line: usize,
    code: &'static str,
    truth: Truth,
    group: String,
}

// --- hand-written cases ----------------------------------------------------

/// `expect: KV002 KV003` anywhere on a line.
fn markers(line: &str) -> Vec<&'static str> {
    let Some(at) = line.find("expect:") else {
        return Vec::new();
    };
    line[at + "expect:".len()..]
        .split_whitespace()
        .map_while(code_of)
        .collect()
}

fn code_of(s: &str) -> Option<&'static str> {
    match s.trim_end_matches([',', ';']) {
        "KV001" => Some("KV001"),
        "KV002" => Some("KV002"),
        "KV003" => Some("KV003"),
        _ => None,
    }
}

fn group_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let ext = name.rsplit('.').next().unwrap_or("");
    if name == ".env" || name.starts_with(".env.") || ext == "env" {
        return "env".into();
    }
    match ext {
        "py" => "python",
        "ipynb" => "notebook",
        "yml" | "yaml" => "yaml",
        "ini" | "cfg" => "ini",
        "toml" => "toml",
        "json" => "json",
        _ => "other",
    }
    .into()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// `expect-skip: unreadable` anywhere in a file.
fn expected_skip(text: &str) -> Option<String> {
    let at = text.find("expect-skip:")?;
    text[at + "expect-skip:".len()..]
        .split_whitespace()
        .next()
        .map(str::to_string)
}

/// Copy `cases/` into `root`, expanding placeholders, and collect the labels
/// and the expected `(path, reason)` skips.
fn materialize_cases(root: &Path, cases: &mut Vec<Case>, skips: &mut BTreeSet<(String, String)>) {
    let src_root = Path::new(CORPUS).join("cases");
    let mut files = Vec::new();
    walk(&src_root, &mut files);

    for src in files {
        let rel = src.strip_prefix(&src_root).unwrap();
        let rel_str = format!("cases/{}", rel.to_string_lossy().replace('\\', "/"));
        if rel_str.ends_with(".expect") {
            continue;
        }
        let bytes = std::fs::read(&src).unwrap();
        // Non-UTF-8 cases (a latin-1 file) are copied byte for byte; they
        // hold no placeholders.
        let (content, text) = match String::from_utf8(bytes.clone()) {
            Ok(s) => {
                let e = expand(&s);
                (e.clone().into_bytes(), e)
            }
            Err(_) => (bytes.clone(), String::from_utf8_lossy(&bytes).into_owned()),
        };
        let dest = root.join(&rel_str);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, content).unwrap();
        if let Some(reason) = expected_skip(&text) {
            skips.insert((rel_str.clone(), reason));
        }

        let mut labelled: Vec<(usize, &'static str)> = Vec::new();
        let sidecar = src.with_file_name(format!(
            "{}.expect",
            src.file_name().unwrap().to_string_lossy()
        ));
        if sidecar.exists() {
            for l in std::fs::read_to_string(&sidecar).unwrap().lines() {
                let l = l.split('#').next().unwrap().trim();
                if l.is_empty() {
                    continue;
                }
                let mut parts = l.split_whitespace();
                let line: usize = parts.next().unwrap().parse().unwrap();
                let code = code_of(parts.next().unwrap())
                    .unwrap_or_else(|| panic!("{}: bad code in {l:?}", sidecar.display()));
                labelled.push((line, code));
            }
        } else {
            for (i, line) in text.lines().enumerate() {
                for code in markers(line) {
                    labelled.push((i + 1, code));
                }
            }
        }
        let group = group_of(&rel_str);
        for (line, code) in labelled {
            cases.push(Case {
                id: format!("{rel_str}:{line}:{code}"),
                path: rel_str.clone(),
                line,
                code,
                truth: Truth::Report,
                group: group.clone(),
            });
        }
    }
}

// --- the matrix ------------------------------------------------------------

/// A syntactic form: file it lands in, lines before the first case, and how
/// one case is written. Keys repeat across cases on purpose: a suffix to make
/// them unique (`secret_name_12`) would change what the name rules see. `{name}`, `{NAME}`, `{value}`, `{head}`/`{tail}` (the
/// value split in two) and `{i}` are substituted.
struct Form {
    id: &'static str,
    file: &'static str,
    header: &'static [&'static str],
    template: &'static str,
}

const FORMS: &[Form] = &[
    Form {
        id: "py-assign",
        file: "assign.py",
        header: &[],
        template: "{name} = \"{value}\"",
    },
    Form {
        id: "py-annotated",
        file: "annotated.py",
        header: &[],
        template: "{name}: str = \"{value}\"",
    },
    Form {
        id: "py-dict",
        file: "dict.py",
        header: &[],
        template: "cfg_{i} = {\"{name}\": \"{value}\"}",
    },
    Form {
        id: "py-kwarg",
        file: "kwarg.py",
        header: &[],
        template: "connect_{i}({name}=\"{value}\")",
    },
    Form {
        id: "py-subscript",
        file: "subscript.py",
        header: &[],
        template: "settings[\"{name}\"] = \"{value}\"",
    },
    Form {
        id: "py-getenv-fallback",
        file: "getenv.py",
        header: &["import os"],
        template: "v_{i} = os.getenv(\"{NAME}\", \"{value}\")",
    },
    Form {
        id: "py-concat",
        file: "concat.py",
        header: &[],
        template: "{name} = \"{head}\" \"{tail}\"",
    },
    Form {
        id: "py-compare",
        file: "compare.py",
        header: &[],
        template: "if {name} == \"{value}\": pass",
    },
    Form {
        id: "yaml",
        file: "config.yml",
        header: &[],
        template: "{name}: \"{value}\"",
    },
    Form {
        id: "env",
        file: ".env",
        header: &[],
        template: "{NAME}={value}",
    },
    Form {
        id: "ini",
        file: "settings.ini",
        header: &["[matrix]"],
        template: "{name} = {value}",
    },
    Form {
        id: "toml",
        file: "settings.toml",
        header: &["[matrix]"],
        template: "{name} = \"{value}\"",
    },
    Form {
        id: "json",
        file: "settings.json",
        header: &["{"],
        template: "  \"{name}\": \"{value}\",",
    },
];

fn tsv(name: &str) -> Vec<Vec<String>> {
    let path = Path::new(CORPUS).join("matrix").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| l.split('\t').map(|c| c.trim().to_string()).collect())
        .collect()
}

/// How a name is spelled as an environment variable: `privateKey` ->
/// `PRIVATE_KEY`, as people write them, not `PRIVATEKEY`.
fn env_name(name: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_uppercase() && prev_lower {
            out.push('_');
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        out.extend(c.to_uppercase());
    }
    out
}

/// The labelling rule, stated once: a credential shape is a credential under
/// any name; a real-looking literal is one under a secret name; placeholders
/// and references never are.
fn truth(name_is_secret: bool, value_class: &str) -> Truth {
    match value_class {
        "token" => Truth::Report,
        "opaque" | "descriptive" if name_is_secret => Truth::Report,
        "opaque" | "descriptive" | "placeholder" | "templated" => Truth::Clean,
        other => panic!("values.tsv: unknown class {other:?}"),
    }
}

fn materialize_matrix(root: &Path, cases: &mut Vec<Case>) {
    let names = tsv("names.tsv");
    let values = tsv("values.tsv");
    for form in FORMS {
        let mut lines: Vec<String> = form.header.iter().map(|s| s.to_string()).collect();
        let mut i = 0;
        for n in &names {
            let (name, label) = (&n[0], n[1].as_str());
            let secret = match label {
                "secret" => true,
                "not" => false,
                other => panic!("names.tsv: {name}: label must be secret|not, got {other:?}"),
            };
            for v in &values {
                let (value_id, class) = (&v[0], v[2].as_str());
                // Expand before splitting: `{head}`/`{tail}` must split the
                // fake token, not the placeholder that names it.
                let value = &expand(&v[1]);
                i += 1;
                let mid = value.len() / 2;
                let line = form
                    .template
                    .replace("{name}", name)
                    .replace("{NAME}", &env_name(name))
                    .replace("{head}", &value[..mid])
                    .replace("{tail}", &value[mid..])
                    .replace("{value}", value)
                    .replace("{i}", &i.to_string());
                lines.push(line);
                cases.push(Case {
                    id: format!("matrix:{}:{name}:{value_id}", form.id),
                    path: format!("matrix/{}", form.file),
                    line: lines.len(),
                    code: "KV002",
                    truth: truth(secret, class),
                    group: format!("matrix/{}", form.id),
                });
            }
        }
        if form.id == "json" {
            // Valid JSON: no trailing comma, closing brace.
            if let Some(last) = lines.last_mut() {
                last.pop();
            }
            lines.push("}".into());
        }
        let dest = root.join("matrix").join(form.file);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(dest, expand(&(lines.join("\n") + "\n"))).unwrap();
    }
}

// --- running and comparing -------------------------------------------------

/// `(path, line, code)` of every finding kv-cli reports for `root`, and the
/// `(path, reason)` of every file it skipped.
type Outcome = (
    BTreeSet<(String, usize, String)>,
    BTreeSet<(String, String)>,
);

fn findings(root: &Path) -> Outcome {
    let out = Command::new(env!("CARGO_BIN_EXE_kv-cli"))
        .args([
            "audit",
            ".",
            "--format",
            "json",
            "--config",
            ".kovallent.yaml",
        ])
        .current_dir(root)
        .output()
        .expect("kv-cli runs");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "no JSON payload ({e}): {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let findings = payload["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_string(),
                f["line"].as_u64().unwrap() as usize,
                f["code"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let skipped = payload["run"]["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["path"].as_str().unwrap().to_string(),
                s["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    (findings, skipped)
}

#[derive(Default)]
struct Tally {
    tp: usize,
    fp: usize,
    fn_: usize,
}

impl Tally {
    fn precision(&self) -> f64 {
        ratio(self.tp, self.tp + self.fp)
    }
    fn recall(&self) -> f64 {
        ratio(self.tp, self.tp + self.fn_)
    }
}

fn ratio(n: usize, d: usize) -> f64 {
    if d == 0 {
        1.0
    } else {
        n as f64 / d as f64
    }
}

/// Known deviations: `FP|FN <tab> case id <tab> note`.
fn read_baseline() -> BTreeMap<String, (String, String)> {
    let path = Path::new(CORPUS).join("baseline.tsv");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut parts = l.splitn(3, '\t');
            let kind = parts.next().unwrap().to_string();
            let id = parts.next().unwrap_or("").to_string();
            let note = parts.next().unwrap_or("").to_string();
            (id, (kind, note))
        })
        .collect()
}

fn write_baseline(deviations: &BTreeMap<String, String>, old: &BTreeMap<String, (String, String)>) {
    let mut out = String::from(
        "# Known deviations between kv-cli and the corpus labels.\n\
         # FP: reported, but labelled clean. FN: labelled as a finding, not reported.\n\
         # Generated by `KV_CORPUS_BLESS=1 cargo test --test corpus`; notes are kept.\n",
    );
    for (id, kind) in deviations {
        let note = old.get(id).map(|(_, n)| n.as_str()).unwrap_or("");
        out.push_str(&format!("{kind}\t{id}\t{note}\n"));
    }
    std::fs::write(Path::new(CORPUS).join("baseline.tsv"), out).unwrap();
}

#[test]
fn corpus_matches_labels_up_to_the_recorded_baseline() {
    let root = std::env::temp_dir().join(format!("kvcli-corpus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    // Built-in defaults, pinned so no contract above the temp dir applies.
    std::fs::write(root.join(".kovallent.yaml"), "version: 1\n").unwrap();

    let mut cases = Vec::new();
    let mut expected_skips = BTreeSet::new();
    materialize_cases(&root, &mut cases, &mut expected_skips);
    materialize_matrix(&root, &mut cases);
    let (mut reported, skipped) = findings(&root);
    let _ = std::fs::remove_dir_all(&root);

    // Skips are few and binary, so they are asserted outright rather than
    // ratcheted: a file kv-cli could not analyse is never a known deviation.
    assert_eq!(
        skipped, expected_skips,
        "skipped files differ from the `expect-skip:` labels"
    );

    let mut ids = BTreeSet::new();
    for c in &cases {
        assert!(ids.insert(c.id.clone()), "duplicate case id {}", c.id);
    }

    // Score labelled cases, consuming the findings they account for.
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    let mut deviations: BTreeMap<String, String> = BTreeMap::new();
    for c in &cases {
        let key = (c.path.clone(), c.line, c.code.to_string());
        let got = reported.remove(&key);
        let t = tallies.entry(c.group.clone()).or_default();
        match (c.truth, got) {
            (Truth::Report, true) => t.tp += 1,
            (Truth::Report, false) => {
                t.fn_ += 1;
                deviations.insert(c.id.clone(), "FN".into());
            }
            (Truth::Clean, true) => {
                t.fp += 1;
                deviations.insert(c.id.clone(), "FP".into());
            }
            (Truth::Clean, false) => {}
        }
    }
    // Whatever is left was reported on a line nobody labelled.
    for (path, line, code) in reported {
        let id = format!("{path}:{line}:{code}");
        tallies.entry(group_of(&path)).or_default().fp += 1;
        deviations.insert(id, "FP".into());
    }

    // The report: `make corpus` shows it.
    let total = tallies.values().fold(Tally::default(), |mut a, t| {
        a.tp += t.tp;
        a.fp += t.fp;
        a.fn_ += t.fn_;
        a
    });
    println!(
        "\ncorpus: {} labelled cases, {} expected skips",
        cases.len(),
        expected_skips.len()
    );
    println!(
        "{:<26} {:>5} {:>5} {:>5} {:>10} {:>8}",
        "group", "TP", "FP", "FN", "precision", "recall"
    );
    for (group, t) in tallies
        .iter()
        .chain(std::iter::once((&"TOTAL".to_string(), &total)))
    {
        println!(
            "{group:<26} {:>5} {:>5} {:>5} {:>9.1}% {:>7.1}%",
            t.tp,
            t.fp,
            t.fn_,
            100.0 * t.precision(),
            100.0 * t.recall()
        );
    }

    let baseline = read_baseline();
    if std::env::var_os("KV_CORPUS_BLESS").is_some() {
        write_baseline(&deviations, &baseline);
        println!("baseline rewritten: {} deviations", deviations.len());
        return;
    }

    let mut problems = Vec::new();
    for (id, kind) in &deviations {
        match baseline.get(id) {
            Some((k, _)) if k == kind => {}
            _ => problems.push(format!("  new {kind}: {id}")),
        }
    }
    for (id, (kind, _)) in &baseline {
        if deviations.get(id) != Some(kind) {
            problems.push(format!("  resolved {kind} (remove from baseline): {id}"));
        }
    }
    assert!(
        problems.is_empty(),
        "corpus outcomes changed against tests/corpus/baseline.tsv:\n{}\n\
         If intended, rerun with KV_CORPUS_BLESS=1 and review the diff.",
        problems.join("\n")
    );
}
