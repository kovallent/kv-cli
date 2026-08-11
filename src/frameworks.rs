//! Built-in framework profiles.
//!
//! A profile contributes three things, all scoped to files where the framework
//! is actually in use:
//!
//! 1. **Signatures the framework owns.** A `@dlt.table` function is called by
//!    Delta Live Tables, not by us; injecting a contract parameter would break
//!    the pipeline. These are excluded from KV001 even when the user's name
//!    patterns would otherwise match.
//! 2. **Credential shapes** specific to the stack (KV002).
//! 3. **Infrastructure identifiers** that should vary by environment (KV003).
//!
//! Scoping matters: `*account*` or `*warehouse*` would be noisy as global
//! rules, but in a file that imports `snowflake` they are precise.

use crate::python::{Analysis, FunctionDef};

pub struct FrameworkProfile {
    pub name: &'static str,
    pub summary: &'static str,
    /// Root module names whose import implies this framework is in use.
    pub detect_imports: &'static [&'static str],
    /// Decorators whose signature the framework controls. Functions carrying
    /// these are exempt from KV001 - the framework calls them, so we cannot
    /// change how they are called.
    pub owned_decorators: &'static [&'static str],
    /// Decorators that bring a function *into* KV001 scope. Declaring these on
    /// the profile means the behaviour does not depend on the user's contract
    /// happening to list the same decorator name.
    pub governed_decorators: &'static [&'static str],
    /// May `kv-cli fix` insert parameters into functions this profile governs?
    /// False where the call sites live in user code and would need the value
    /// threaded through as well - inserting a default alone would leave the
    /// parameter silently reading `"dev"` in every environment.
    pub governed_auto_fix: bool,
    /// Signature shapes the framework controls, as `(name, leading params)`.
    pub owned_signatures: &'static [(&'static str, &'static [&'static str])],
    pub secret_keys: &'static [&'static str],
    pub secret_values: &'static [(&'static str, &'static str)],
    pub infra_keys: &'static [&'static str],
    pub infra_values: &'static [(&'static str, &'static str)],
}

pub const PROFILES: &[FrameworkProfile] = &[
    FrameworkProfile {
        name: "dbt",
        summary: "dbt Python models and profiles.yml credentials",
        detect_imports: &["dbt"],
        owned_decorators: &[],
        // `def model(dbt, session)` is dbt's fixed entrypoint contract.
        owned_signatures: &[("model", &["dbt", "session"])],
        governed_decorators: &[],
        governed_auto_fix: true,
        secret_keys: &["*private_key_passphrase*", "*client_secret*"],
        secret_values: &[],
        // dbt's per-target config is *supposed* to name warehouses and
        // schemas, so this profile adds no infrastructure rules.
        infra_keys: &[],
        infra_values: &[],
    },
    FrameworkProfile {
        name: "polars",
        summary: "Polars object-store paths and storage_options credentials",
        detect_imports: &["polars"],
        owned_decorators: &[],
        owned_signatures: &[],
        governed_decorators: &[],
        governed_auto_fix: true,
        secret_keys: &["*aws_secret*", "*azure_storage_key*", "*sas_token*"],
        secret_values: &[],
        infra_keys: &["*bucket*", "*source_path*", "*sink_path*"],
        infra_values: &[],
    },
    FrameworkProfile {
        name: "flink",
        summary: "PyFlink UDF signatures and connector endpoints",
        detect_imports: &["pyflink"],
        owned_decorators: &[
            "udf",
            "udtf",
            "udaf",
            "pyflink.table.udf",
            "pyflink.table.udtf",
            "pyflink.table.udaf",
        ],
        owned_signatures: &[],
        governed_decorators: &[],
        governed_auto_fix: true,
        secret_keys: &["*sasl*password*", "*ssl*key*password*"],
        secret_values: &[],
        infra_keys: &[
            "*bootstrap*servers*",
            "*jobmanager*",
            "*taskmanager*",
            "*checkpoint*dir*",
            "*savepoint*path*",
        ],
        infra_values: &[("jdbc_url", r"\bjdbc:[a-z0-9]+://[^\s\x22']+")],
    },
    FrameworkProfile {
        name: "databricks",
        summary: "Delta Live Tables signatures, workspace URLs and PATs",
        detect_imports: &["dlt", "databricks", "pyspark"],
        owned_decorators: &[
            "dlt.table",
            "dlt.view",
            "dlt.expect",
            "dlt.expect_all",
            "dlt.expect_or_drop",
            "dlt.expect_or_fail",
            "table",
            "view",
            "pandas_udf",
        ],
        owned_signatures: &[],
        governed_decorators: &[],
        governed_auto_fix: true,
        secret_keys: &["*databricks*token*"],
        // Databricks personal access tokens.
        secret_values: &[("databricks_pat", r"\bdapi[0-9a-f]{32}\b")],
        infra_keys: &[
            "*cluster_id*",
            "*workspace_url*",
            "*catalog*",
            "*warehouse_id*",
            "*instance_pool*",
        ],
        infra_values: &[
            (
                "databricks_workspace_url",
                r"https://(?:[a-z0-9-]+\.cloud\.databricks\.com|adb-\d+\.\d+\.azuredatabricks\.net)",
            ),
            ("databricks_cluster_id", r"\b\d{4}-\d{6}-[a-z0-9]{8}\b"),
            ("dbfs_path", r"\bdbfs:/[^\s\x22']+"),
        ],
    },
    FrameworkProfile {
        name: "snowpark",
        summary: "Snowpark UDF/sproc signatures and session config",
        detect_imports: &["snowflake"],
        owned_decorators: &[
            "sproc",
            "udf",
            "udtf",
            "udaf",
            "pandas_udf",
            "snowflake.snowpark.functions.udf",
            "snowflake.snowpark.functions.sproc",
        ],
        owned_signatures: &[],
        governed_decorators: &[],
        governed_auto_fix: true,
        secret_keys: &["*private_key*", "*passphrase*"],
        secret_values: &[],
        // These appear as keys in `Session.builder.configs({...})`.
        infra_keys: &[
            "*warehouse*",
            "*account*",
            "*role*",
            "*database*",
            "*stage*",
        ],
        infra_values: &[(
            "snowflake_account_url",
            r"\b[a-z0-9][a-z0-9._-]*\.snowflakecomputing\.com",
        )],
    },
    FrameworkProfile {
        name: "airflow",
        summary: "Airflow DAG/task signatures, connection IDs and Fernet keys",
        detect_imports: &["airflow"],
        // Airflow instantiates the DAG function, and its parameters become DAG
        // params surfaced in the UI and in `{{ params.* }}`.
        owned_decorators: &[
            "dag",
            "task_group",
            "setup",
            "teardown",
            "asset",
            "dataset",
            "airflow.decorators.dag",
            "airflow.decorators.task_group",
        ],
        // TaskFlow tasks are called from your own DAG body, so the contract
        // parameter can be threaded through by hand at the call site.
        governed_decorators: &["task", "airflow.decorators.task"],
        // Reported, never rewritten: `extract_orders("s3://...")` does not
        // pass the parameter, so an inserted default would read "dev" forever.
        governed_auto_fix: false,
        owned_signatures: &[],
        secret_keys: &["*fernet_key*", "*webserver_secret*", "*smtp_password*"],
        secret_values: &[],
        // `dag_id` is deliberately absent: every DAG has one and almost none
        // of them vary by environment, so it is pure noise.
        infra_keys: &["*conn_id*", "*pool*", "*queue*"],
        infra_values: &[],
    },
];

pub fn by_name(name: &str) -> Option<&'static FrameworkProfile> {
    PROFILES.iter().find(|p| p.name == name)
}

impl FrameworkProfile {
    /// Is this framework in use in the analysed file?
    pub fn detected(&self, a: &Analysis) -> bool {
        if self
            .detect_imports
            .iter()
            .any(|m| a.imports.modules.iter().any(|seen| seen == m))
        {
            return true;
        }
        // dbt injects `dbt` and `session`; nothing is imported.
        a.functions.iter().any(|f| self.owns_signature(f))
    }

    /// Does the framework control this function's signature?
    pub fn owns_signature(&self, f: &FunctionDef) -> bool {
        if f.decorators.iter().any(|d| {
            self.owned_decorators
                .iter()
                .any(|owned| d == owned || d.rsplit('.').next().is_some_and(|last| last == *owned))
        }) {
            return true;
        }
        self.owned_signatures.iter().any(|(name, leading)| {
            f.name == *name
                && leading.len() <= f.params.len()
                && leading
                    .iter()
                    .zip(f.params.iter())
                    .all(|(want, got)| got.name == *want)
        })
    }

    /// Does the framework put this function under the parameter contract?
    pub fn governs_signature(&self, f: &FunctionDef) -> bool {
        decorator_matches(f, self.governed_decorators)
    }
}

/// Match a decorator by full dotted path or by its final segment, so `task`
/// matches both `@task` and `@airflow.decorators.task`.
fn decorator_matches(f: &FunctionDef, names: &[&str]) -> bool {
    f.decorators.iter().any(|d| {
        names
            .iter()
            .any(|want| d == want || d.rsplit('.').next().is_some_and(|last| last == *want))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::python::analyze;

    #[test]
    fn every_profile_regex_compiles() {
        for p in PROFILES {
            for (name, re) in p.secret_values.iter().chain(p.infra_values) {
                regex::Regex::new(re)
                    .unwrap_or_else(|e| panic!("{}/{name} failed to compile: {e}", p.name));
            }
        }
    }

    #[test]
    fn detects_by_import() {
        let a = analyze("import polars as pl\n");
        assert!(by_name("polars").unwrap().detected(&a));
        assert!(!by_name("flink").unwrap().detected(&a));

        let a = analyze("from snowflake.snowpark import Session\n");
        assert!(by_name("snowpark").unwrap().detected(&a));
    }

    #[test]
    fn detects_dbt_by_signature_without_any_import() {
        let a = analyze("def model(dbt, session):\n    return session.table(\"x\")\n");
        assert!(by_name("dbt").unwrap().detected(&a));
        assert!(by_name("dbt").unwrap().owns_signature(&a.functions[0]));
    }

    #[test]
    fn dbt_signature_must_match_exactly() {
        // A user function that merely happens to be called `model`.
        let a = analyze("def model(x, y):\n    pass\n");
        assert!(!by_name("dbt").unwrap().owns_signature(&a.functions[0]));
    }

    #[test]
    fn owned_decorators_match_bare_and_dotted() {
        let a = analyze("@dlt.table\ndef daily_job():\n    pass\n");
        assert!(by_name("databricks")
            .unwrap()
            .owns_signature(&a.functions[0]));

        let a = analyze("from dlt import table\n@table\ndef daily_job():\n    pass\n");
        assert!(by_name("databricks")
            .unwrap()
            .owns_signature(&a.functions[0]));

        let a = analyze("@udf(result_type=DataTypes.INT())\ndef run_calc(x):\n    pass\n");
        assert!(by_name("flink").unwrap().owns_signature(&a.functions[0]));
    }

    #[test]
    fn databricks_pat_pattern() {
        let re = regex::Regex::new(by_name("databricks").unwrap().secret_values[0].1).unwrap();
        assert!(re.is_match("dapi1234567890abcdef1234567890abcdef"));
        assert!(!re.is_match("dapi-not-a-token"));
    }
}
