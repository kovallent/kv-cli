//! Fix previews as redacted unified diffs (ADR 0006, decisions 3 and 4).
//!
//! A diff's removed line is exactly where a credential would leak, so each
//! side is rebuilt first with every span the audit flags replaced by its
//! redaction, and only then diffed. The flagged spans come from the same
//! audit `kv-cli audit` runs, over the old source and over the planned one.

use crate::audit::{redact, Engine, Finding, FindingKind, Flagged};
use crate::python;
use std::ops::Range;

/// `src` with every span a KV002 finding flags replaced by its redaction, and
/// the flagged values themselves, for scrubbing anything else built from it.
pub fn redact_python(engine: &Engine, rel: &str, src: &str) -> (String, Vec<Flagged>) {
    let analysis = python::analyze(src);
    let ctx = engine.context(&analysis);
    let flagged = flagged_values(&ctx.audit(rel, src, &analysis));
    (redact_spans(src, &flagged), flagged)
}

/// The flagged values of a run's KV002 findings.
pub fn flagged_values(findings: &[Finding]) -> Vec<Flagged> {
    findings
        .iter()
        .filter_map(|f| match &f.kind {
            FindingKind::HardcodedSecret { flagged, .. } => Some(flagged.clone()),
            _ => None,
        })
        .collect()
}

/// Replace each flagged span in `src` with its redaction. Overlapping spans
/// are merged, so nothing inside either survives.
pub fn redact_spans(src: &str, flagged: &[Flagged]) -> String {
    let mut spans: Vec<(Range<usize>, &str)> = flagged
        .iter()
        .filter_map(|f| f.span.clone().map(|s| (s, f.text.as_str())))
        .filter(|(s, _)| s.start < s.end && s.end <= src.len())
        .filter(|(s, _)| src.is_char_boundary(s.start) && src.is_char_boundary(s.end))
        .collect();
    spans.sort_by_key(|(s, _)| (s.start, std::cmp::Reverse(s.end)));
    let mut out = String::with_capacity(src.len());
    let mut at = 0;
    let mut i = 0;
    while i < spans.len() {
        let (mut span, text) = spans[i].clone();
        let outer_end = span.end;
        // Absorb every span that starts inside this one. When they all lie
        // within it, it is the outer finding's value that is redacted;
        // otherwise the merged stretch of source is.
        let mut j = i + 1;
        while j < spans.len() && spans[j].0.start < span.end {
            span.end = span.end.max(spans[j].0.end);
            j += 1;
        }
        if span.start >= at {
            out.push_str(&src[at..span.start]);
            out.push_str(&redact(if span.end == outer_end {
                text
            } else {
                &src[span.clone()]
            }));
            at = span.end;
        }
        i = j;
    }
    out.push_str(&src[at..]);
    out
}

/// A unified diff of `old` against `new`, labelled `a/<path>` and `b/<path>`,
/// with three lines of context. Empty when they are the same.
pub fn unified(path: &str, old: &str, new: &str) -> String {
    const CONTEXT: usize = 3;
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let ops = myers(&a, &b);
    if ops.iter().all(|o| matches!(o, Op::Same(..))) {
        return String::new();
    }

    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    // Group the edit script into hunks: runs of changes with up to
    // 2 * CONTEXT unchanged lines between them, plus CONTEXT on each side.
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, o)| !matches!(o, Op::Same(..)))
        .map(|(i, _)| i)
        .collect();
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for &i in &changed {
        match groups.last_mut() {
            Some((_, end)) if i <= *end + 2 * CONTEXT + 1 => *end = i,
            _ => groups.push((i, i)),
        }
    }
    for (first, last) in groups {
        let start = first.saturating_sub(CONTEXT);
        let end = (last + CONTEXT + 1).min(ops.len());
        let (old_start, new_start) = position(&ops, start);
        let (mut old_len, mut new_len) = (0, 0);
        let mut body = String::new();
        for op in &ops[start..end] {
            let (mark, line) = match op {
                Op::Same(i, _) => {
                    old_len += 1;
                    new_len += 1;
                    (' ', a[*i])
                }
                Op::Del(i) => {
                    old_len += 1;
                    ('-', a[*i])
                }
                Op::Ins(j) => {
                    new_len += 1;
                    ('+', b[*j])
                }
            };
            body.push(mark);
            body.push_str(line);
            if !line.ends_with('\n') {
                body.push_str("\n\\ No newline at end of file\n");
            }
        }
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            range(old_start, old_len),
            range(new_start, new_len)
        ));
        out.push_str(&body);
    }
    out
}

/// `start,len` as a hunk header writes it, 1-based; `start-1,0` for empty.
fn range(start: usize, len: usize) -> String {
    if len == 0 {
        format!("{start},0")
    } else if len == 1 {
        format!("{}", start + 1)
    } else {
        format!("{},{len}", start + 1)
    }
}

/// The 0-based old and new line numbers at which `ops[at]` begins.
fn position(ops: &[Op], at: usize) -> (usize, usize) {
    let (mut o, mut n) = (0, 0);
    for op in &ops[..at] {
        match op {
            Op::Same(..) => {
                o += 1;
                n += 1;
            }
            Op::Del(_) => o += 1,
            Op::Ins(_) => n += 1,
        }
    }
    (o, n)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// Line `.0` of the old text, line `.1` of the new.
    Same(usize, usize),
    Del(usize),
    Ins(usize),
}

/// Myers' O((N+M)D) shortest edit script. Fix plans change a handful of
/// lines, so D - and the trace kept for backtracking - stays small.
fn myers(a: &[&str], b: &[&str]) -> Vec<Op> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    let offset = max as isize + 1;
    let mut v = vec![0isize; 2 * max + 3];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    'search: for d in 0..=max as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let idx = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) {
                v[idx + 1]
            } else {
                v[idx - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x >= n && y >= m {
                trace.push(v.clone());
                break 'search;
            }
            k += 2;
        }
    }

    // Walk the trace back from (n, m) to (0, 0).
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..trace.len() - 1).rev() {
        let v = &trace[d];
        let d = d as isize;
        let k = x - y;
        let prev_k =
            if k == -d || (k != d && v[(k - 1 + offset) as usize] < v[(k + 1 + offset) as usize]) {
                k + 1
            } else {
                k - 1
            };
        let prev_x = v[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push(Op::Same(x as usize, y as usize));
        }
        if x == prev_x {
            y -= 1;
            ops.push(Op::Ins(y as usize));
        } else {
            x -= 1;
            ops.push(Op::Del(x as usize));
        }
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        ops.push(Op::Same(x as usize, y as usize));
    }
    ops.reverse();
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applying the edit script to `a` must give `b`, whatever the input.
    fn check(a: &str, b: &str) {
        let al: Vec<&str> = a.split_inclusive('\n').collect();
        let bl: Vec<&str> = b.split_inclusive('\n').collect();
        let ops = myers(&al, &bl);
        let rebuilt: String = ops
            .iter()
            .filter_map(|o| match o {
                Op::Same(i, _) => Some(al[*i]),
                Op::Ins(j) => Some(bl[*j]),
                Op::Del(_) => None,
            })
            .collect();
        assert_eq!(rebuilt, b);
        let kept: String = ops
            .iter()
            .filter_map(|o| match o {
                Op::Same(i, _) | Op::Del(i) => Some(al[*i]),
                Op::Ins(_) => None,
            })
            .collect();
        assert_eq!(kept, a);
    }

    #[test]
    fn edit_scripts_rebuild_both_sides() {
        check("", "");
        check("a\n", "");
        check("", "a\n");
        check("a\nb\nc\n", "a\nb\nc\n");
        check("a\nb\nc\n", "a\nx\nc\n");
        check("a\nb\nc\nd\ne\n", "b\nc\nx\ne\nf\n");
        check("a\nb", "a\nb\n");
        let long_a: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let long_b = long_a
            .replace("line 50\n", "changed\n")
            .replace("line 150\n", "");
        check(&long_a, &long_b);
    }

    #[test]
    fn a_unified_diff_has_headers_hunks_and_context() {
        let old: String = (1..=10).map(|i| format!("{i}\n")).collect();
        let new = old.replace("5\n", "five\n");
        let d = unified("x.py", &old, &new);
        assert_eq!(
            d,
            "--- a/x.py\n+++ b/x.py\n@@ -2,7 +2,7 @@\n 2\n 3\n 4\n-5\n+five\n 6\n 7\n 8\n"
        );
        assert_eq!(unified("x.py", &old, &old), "");
    }

    #[test]
    fn distant_changes_get_separate_hunks() {
        let old: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        let new = old
            .replace("line 3\n", "three\n")
            .replace("line 27\n", "twenty-seven\n");
        let d = unified("x.py", &old, &new);
        assert_eq!(d.matches("@@ -").count(), 2, "{d}");
    }

    #[test]
    fn overlapping_spans_are_redacted_once_and_whole() {
        let src = "a = \"s3cr3t-production-pw\"\n";
        let start = src.find('"').unwrap();
        let end = src.rfind('"').unwrap() + 1;
        let flagged = vec![
            Flagged {
                text: "s3cr3t-production-pw".into(),
                span: Some(start..end),
                parts: Vec::new(),
            },
            Flagged {
                text: "s3cr3t".into(),
                span: Some(start + 1..start + 7),
                parts: Vec::new(),
            },
        ];
        let out = redact_spans(src, &flagged);
        assert!(!out.contains("s3cr3t-"), "{out}");
        assert_eq!(out, format!("a = {}\n", redact("s3cr3t-production-pw")));
    }
}
