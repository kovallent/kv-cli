//! dbt SQL models: finding them, and reading them without rendering Jinja.
//! See docs/design/sql-models.md.

pub mod discover;
pub mod neutralise;
pub mod parse;
pub mod rules;
