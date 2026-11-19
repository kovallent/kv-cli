//! Compiles the BigQuery grammar that ./fetch.sh extracts into grammar-bq/:
//! tree-sitter-sql-bigquery 0.8.0's Rust crate pins tree-sitter < 0.23 and
//! cannot be linked beside 0.26, but its parser.c (ABI 14) loads in 0.26.
fn main() {
    let dir = std::path::Path::new("grammar-bq/src");
    println!("cargo:rerun-if-changed=grammar-bq/src/parser.c");
    assert!(dir.join("parser.c").exists(), "run ./fetch.sh first");
    let mut b = cc::Build::new();
    b.include(dir).file(dir.join("parser.c")).warnings(false);
    if dir.join("scanner.c").exists() {
        b.file(dir.join("scanner.c"));
    }
    b.compile("tree_sitter_sql_bigquery_fetched");
}
