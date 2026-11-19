//! dbt SQL models: finding them, and reading them without rendering Jinja.
//! See docs/design/sql-models.md.

// Discovery and the neutraliser land before the parser and the rules that
// call them (one PR each into `sql-stage-1`); until then only tests use them.
#![cfg_attr(not(test), allow(dead_code))]

pub mod discover;
pub mod neutralise;
