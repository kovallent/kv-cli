# SQL corpus

The dbt models kv-cli's SQL support is tested against: the neutraliser's
invariants, the parse-coverage check and, from the rules onward, zero findings
on unseeded models. See [`docs/design/sql-models.md`](../../docs/design/sql-models.md),
owner decision 7 and exit criterion 5.

**Permissive plus synthetic only.** Nothing else may be committed here.

- **`permissive/<owner>-<project>/`** holds models copied unmodified from a
  project with an MIT, Apache-2.0 or BSD licence at its repository root. Each
  folder carries that project's `LICENSE` verbatim and a `SOURCE.md` naming
  the repository, the pinned commit and each model's original path. Paths
  inside the folder are the project's own.
- **`synthetic/<dialect>/`** holds models written for kv-cli. Each file's
  first line says `SYNTHETIC`, and the comment under it names the syntax it
  exercises.

`manifest.json` lists every project and every model with its dialect. A test
checks that the manifest and the files agree, that each permissive folder has
its `LICENSE` and `SOURCE.md`, and that each synthetic model is labelled.
