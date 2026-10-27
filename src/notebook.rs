//! Jupyter notebooks (`.ipynb`): code cells as Python, mapped back to the
//! notebook's own line numbers.
//!
//! Every code cell is joined into one Python source, so an import in the
//! first cell still informs framework detection in the fifth, and the result
//! is analysed by the same engine as a `.py` file. A finding must point at a
//! line a person can open, so each source line is located in the raw JSON:
//! Jupyter writes one source line per JSON array element, and a forward
//! search from the cell's `"source"` key finds it. Where it cannot be found
//! (a tool that escapes non-ASCII, a cell stored as one string), the finding
//! falls back to the line of the cell's `"source"` key.

use std::collections::HashSet;

#[derive(Debug)]
pub struct Notebook {
    /// All code cells, magics blanked, joined with newlines.
    pub python: String,
    /// For each line of `python` (index = line - 1): the notebook line it came
    /// from, and the 1-based code cell number.
    lines: Vec<(usize, usize)>,
    /// Notebook lines confirmed to hold code-cell source, which the Python
    /// analysis is authoritative for.
    code_lines: HashSet<usize>,
}

impl Notebook {
    /// Notebook line and code cell number for a line of [`Self::python`].
    pub fn locate(&self, python_line: usize) -> (usize, usize) {
        self.lines
            .get(python_line.saturating_sub(1))
            .copied()
            .unwrap_or((1, 0))
    }

    /// True when the Python analysis covers this notebook line.
    pub fn is_code_line(&self, line: usize) -> bool {
        self.code_lines.contains(&line)
    }
}

/// Extract the code cells, or describe why the file is not a notebook.
pub fn extract(raw: &str) -> Result<Notebook, String> {
    let doc: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("not valid JSON: {e}"))?;
    let cells = doc
        .get("cells")
        .and_then(|c| c.as_array())
        .ok_or("no `cells` array (only nbformat 4 is supported)")?;

    let starts: Vec<usize> = std::iter::once(0)
        .chain(raw.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let line_at = |offset: usize| starts.partition_point(|&s| s <= offset);

    let mut nb = Notebook {
        python: String::new(),
        lines: Vec::new(),
        code_lines: HashSet::new(),
    };
    let mut cursor = 0;
    let mut cell_no = 0;
    for cell in cells {
        // Every cell has a `source`; stepping past each one keeps the search
        // aligned with the cell it belongs to.
        let source_key = raw[cursor..].find("\"source\"").map(|i| cursor + i);
        if let Some(at) = source_key {
            cursor = at + "\"source\"".len();
        }
        if cell.get("cell_type").and_then(|t| t.as_str()) != Some("code") {
            continue;
        }
        cell_no += 1;
        let text = match cell.get("source") {
            Some(serde_json::Value::Array(parts)) => {
                parts.iter().filter_map(|p| p.as_str()).collect::<String>()
            }
            Some(serde_json::Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        let fallback = line_at(cursor);
        for line in text.split('\n') {
            let escaped = serde_json::to_string(line).unwrap_or_default();
            let needle = &escaped[1..escaped.len().saturating_sub(1).max(1)];
            let found = (!needle.is_empty())
                .then(|| raw[cursor..].find(needle))
                .flatten()
                .map(|i| cursor + i);
            let nb_line = match found {
                Some(at) => {
                    cursor = at + needle.len();
                    let l = line_at(at);
                    nb.code_lines.insert(l);
                    l
                }
                None => fallback,
            };
            if !nb.lines.is_empty() {
                nb.python.push('\n');
            }
            nb.python.push_str(blank_magic(line));
            nb.lines.push((nb_line, cell_no));
        }
    }
    Ok(nb)
}

/// IPython magics and shell escapes (`%pip install`, `!ls`) are not Python;
/// blanking them keeps the parse clean and the line numbering intact.
fn blank_magic(line: &str) -> &str {
    let t = line.trim_start();
    if t.starts_with('%') || t.starts_with('!') {
        ""
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout Jupyter writes: one source line per array element.
    const NB: &str = r##"{
 "cells": [
  {
   "cell_type": "markdown",
   "metadata": {},
   "source": [
    "# Notes"
   ]
  },
  {
   "cell_type": "code",
   "metadata": {},
   "outputs": [],
   "source": [
    "%pip install polars\n",
    "import polars as pl\n",
    "TOKEN = \"abc\""
   ]
  },
  {
   "cell_type": "code",
   "metadata": {},
   "outputs": [],
   "source": [
    "x = 1"
   ]
  }
 ],
 "metadata": {},
 "nbformat": 4,
 "nbformat_minor": 5
}
"##;

    #[test]
    fn code_cells_become_one_python_source() {
        let nb = extract(NB).unwrap();
        assert_eq!(nb.python, "\nimport polars as pl\nTOKEN = \"abc\"\nx = 1");
    }

    #[test]
    fn python_lines_map_to_notebook_lines_and_cells() {
        let nb = extract(NB).unwrap();
        // `TOKEN = "abc"` is python line 3, notebook line 17, code cell 1.
        assert_eq!(nb.locate(3), (17, 1));
        assert_eq!(nb.locate(4), (25, 2));
        assert!(nb.is_code_line(17));
        assert!(!nb.is_code_line(7), "markdown is not code");
    }

    #[test]
    fn a_cell_stored_as_one_string_maps_to_its_source_key() {
        let raw = r#"{"cells":[{"cell_type":"code","source":"a = 1\nTOKEN = 2"}]}"#;
        let nb = extract(raw).unwrap();
        assert_eq!(nb.python, "a = 1\nTOKEN = 2");
        assert_eq!(nb.locate(2).0, 1);
    }

    #[test]
    fn non_notebooks_are_rejected_with_a_reason() {
        assert!(extract("not json").unwrap_err().contains("JSON"));
        assert!(extract("{\"worksheets\": []}")
            .unwrap_err()
            .contains("cells"));
    }
}
