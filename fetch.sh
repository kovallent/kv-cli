#!/usr/bin/env bash
# Fetches what the spike reads, without committing anyone else's code:
# - each corpus model from its project at the commit in corpus/manifest.json;
# - tree-sitter-sql-bigquery 0.8.0 from crates.io, checked against its
#   published checksum, for its generated parser.c (32 MB, not committed).
set -euo pipefail
cd "$(dirname "$0")"

python3 - <<'PY'
import json, pathlib, urllib.request
for m in json.load(open("corpus/manifest.json")):
    out = pathlib.Path("corpus") / m["file"]
    if out.exists():
        continue
    out.parent.mkdir(parents=True, exist_ok=True)
    url = f"https://raw.githubusercontent.com/{m['repo']}/{m['commit']}/{m['path']}"
    out.write_bytes(urllib.request.urlopen(url).read())
    print("fetched", m["file"])
PY

if [ ! -f grammar-bq/src/parser.c ]; then
  crate=$(mktemp)
  curl -fsSL -o "$crate" https://static.crates.io/crates/tree-sitter-sql-bigquery/tree-sitter-sql-bigquery-0.8.0.crate
  echo "03958a5b10ddcd25ed510cf95a9bbb36cad5837894f4b7743e6ce0e26875e463  $crate" | sha256sum -c -
  mkdir -p grammar-bq
  tar -xzf "$crate" -C grammar-bq --strip-components=1 tree-sitter-sql-bigquery-0.8.0/src
  rm -f "$crate"
fi
echo "ready"
