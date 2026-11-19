"""Print the tables in docs/design/sql-models.md from the spike's results.

Real models (fetched at test time) and synthetic ones (committed) are
reported separately; every rate in the document is over the real ones.
"""
import json
import statistics
import sys

DIALECTS = ["snowflake", "bigquery", "databricks", "postgres"]
parse = json.load(open(sys.argv[1]))
recall = json.load(open(sys.argv[2]))
real = [x for x in parse if not x["synthetic"]]
synthetic = [x for x in parse if x["synthetic"]]


def pct(n, d):
    return f"{n} ({round(100 * n / d)}%)"


print("Parse failures per dialect, real models (refined neutralisation unless noted)")
cols = [
    ("raw Jinja", "sqlparser_raw_ok"),
    ("sqlparser naive", "sqlparser_v1_ok"),
    ("sqlparser", "sqlparser_v2_ok"),
    ("sqlparser generic", "sqlparser_generic_v2_ok"),
    ("sqlparser retry", "sqlparser_any_dialect_v2_ok"),
    ("tokenizer", "tokenizer_v2_ok"),
    ("ts-sequel raw", "ts_sequel_raw_clean"),
    ("ts-sequel naive", "ts_sequel_v1_clean"),
    ("ts-sequel", "ts_sequel_v2_clean"),
    ("ts-bigquery", "ts_bigquery_v2_clean"),
]
print(f"{'dialect':11}{'n':>3}" + "".join(f"{c:>19}" for c, _ in cols))
for d in DIALECTS + ["all"]:
    rows = [x for x in real if d == "all" or x["dialect"] == d]
    cells = "".join(f"{pct(sum(not x[k] for x in rows), len(rows)):>19}" for _, k in cols)
    print(f"{d:11}{len(rows):>3}{cells}")

print("\nFailing files, real models")
for x in real:
    fails = [c for c, k in cols[2:] if not x[k]]
    if fails:
        print(f"  {x['file']:66} {', '.join(fails)}  {x['sqlparser_v2_err'] or ''}")
same = all(x["sqlparser_v2_ok"] == x["sqlparser_generic_v2_ok"] for x in real)
print(f"GenericDialect agrees with the native dialect on every real model: {same}")

errs = [x["ts_sequel_v2_error_share"] for x in real if not x["ts_sequel_v2_clean"]]
print(f"\nts-sequel: median non-whitespace share inside ERROR nodes, failing real files: {100 * statistics.median(errs):.0f}%")
print("  largest:", ", ".join(f"{x['file']} {100 * x['ts_sequel_v2_error_share']:.0f}%"
                              for x in sorted(real, key=lambda x: -x["ts_sequel_v2_error_share"])[:2]))
print("  relations outside errors vs from/join targets counted from tokens:")
for d in DIALECTS:
    rows = [x for x in real if x["dialect"] == d]
    print(f"  {d:10} {sum(x['ts_sequel_v2_relations'] for x in rows):>3} outside, "
          f"{sum(x['ts_sequel_v2_relations_in_error'] for x in rows):>2} inside, "
          f"{sum(sum(x['from_join_targets'].values()) for x in rows):>3} counted")

print("\nfrom/join targets in the real models")
total = {}
for x in real:
    for k, v in x["from_join_targets"].items():
        total[k] = total.get(k, 0) + v
print(f"  {sum(total.values())} targets: {total}")

print("\nSynthetic models (not in any rate)")
for x in synthetic:
    fails = [c for c, k in cols[2:] if not x[k]]
    print(f"  {x['file']:50} fails: {', '.join(fails) or 'none'}  {x['sqlparser_v2_err'] or ''}")

print("\nRecall: seeded positives (GenericDialect, as proposed)")
caught = 0
seeds = [s for s in recall["seeds"] if s["seed"] != "jinja_three_part"]
for s in seeds:
    a = s["as_proposed"]
    caught += a["caught"]
    print(f"  {s['seed']:15} {s['file']:62} parsed={a['parsed']!s:5} caught={a['caught']!s:5} via={a['via']}"
          f"  native: parsed={s['native_dialect']['parsed']} via={s['native_dialect']['via']}")
print(f"  caught {caught} of {len(seeds)}")
for s in recall["seeds"]:
    if s["seed"] == "jinja_three_part":
        print(f"  inside Jinja: as proposed caught={s['as_proposed']['caught']}, "
              f"with literal pass-through caught={s['with_literal_passthrough']['caught']} "
              f"via={s['with_literal_passthrough']['via']}")
other = [h for s in recall["seeds"] for v in ("as_proposed", "with_literal_passthrough", "native_dialect") for h in s[v]["other_hits"]]
print(f"  other hits in seeded copies: {len(other)}")
for key, label in [("unseeded", "Unseeded real models"), ("unseeded_synthetic", "Unseeded synthetic models")]:
    u = recall.get(key)
    if u:
        print(f"{label}: {u['models']} models, {u['fell_back_to_tokens']} via the token fallback, "
              f"{len(u['hits'])} hits (each would be a false positive)")
