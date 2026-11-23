# dbt standard

A preset for dbt projects that enforces KV002, hardcoded credentials, where dbt keeps them:

- **`profiles.yml`**, including fallbacks in Jinja, such as
  `{{ env_var('DBT_PASSWORD', 'fallback') }}`;
- **`dbt_project.yml`**, such as a credential in `vars:`;
- **`.ipynb` notebooks**, exploration notebooks included;
- **Python models**.

It leaves `def model(dbt, session)` alone.

## The preset

Save as `.kovallent.yaml` at the root of the dbt project, or of the repository:

```yaml
version: 1

secrets:
  # The 12 built-in credential shapes, plus Azure storage keys, Databricks tokens
  # and SendGrid keys: a dbt-databricks token is caught under any key name.
  detectors: 2

scan:
  # The defaults, plus what dbt writes or installs: packages and logs.
  exclude:
    - "**/.venv/**"
    - "**/venv/**"
    - "**/.git/**"
    - "**/__pycache__/**"
    - "**/node_modules/**"
    - "**/site-packages/**"
    - "**/build/**"
    - "**/dist/**"
    - "**/target/**"
    - "**/.tox/**"
    - "**/.mypy_cache/**"
    - "**/.kovallent.yaml"
    - "**/dbt_packages/**"
    - "**/dbt_modules/**"
    - "**/logs/**"
```

Fingerprint, used verbatim: `603e17d2f6e286c7`.

`scan.include` keeps its defaults, which already read `*.yml`, `*.yaml`, `*.ipynb` and
`*.py`, so `profiles.yml`, `dbt_project.yml`, notebooks and models are all scanned.

## What is reported

| In your project | What happens |
| --- | --- |
| `password: "{{ env_var('DBT_PASSWORD', 'fallback-pw') }}"` | KV002: the fallback is a hardcoded password. |
| `password: "{{ env_var('DBT_PASSWORD') }}"` | Clean: the value comes from the environment. |
| `private_key_passphrase: "…"`, `client_secret: "…"` | KV002. |
| `private_key_path: /keys/rsa_key.p8` | Clean: a path to a key, not the key. |
| `vars: { vendor_api_key: "…" }` in `dbt_project.yml` | KV002. |
| A `dapi…` token under any key in `profiles.yml` | KV002 (`databricks_pat`, detector set 2). |
| `password="…"` in a notebook's code cell | KV002. |
| `def model(dbt, session):` | Never changed: the dbt profile owns this signature. |

## Notes

- Credentials in YAML and notebooks are listed by `kv-cli fix` as `manual:` items, never
  rewritten. Move each one to `env_var()` without a fallback, or to a secret store.
- This preset leaves KV001 and KV003 at their defaults. KV003 reports pinned relations,
  `USE` statements and object-store paths in SQL models as warnings; see
  [KV003](../rules/KV003.md#dbt-sql-models). To make them gate, set
  `infrastructure.severity: error`.
- Keep `profiles.yml` out of the repository if you can (dbt reads `~/.dbt/` by default).
  kv-cli checks the one you commit.
- Gate in CI with [the example workflow](workflows/kovallent.yml), and run the same checks
  on commit with [the pre-commit config](workflows/pre-commit-config.yaml).
