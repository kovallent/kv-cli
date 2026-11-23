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

use crate::python::{Analysis, FunctionDef, NameOrigin, Names};

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
            "dlt.expect_all_or_drop",
            "dlt.expect_all_or_fail",
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
    /// Is this framework in use in the analysed file? `names` is what the
    /// file binds, from [`Names::of`] on the same analysis.
    pub fn detected(&self, a: &Analysis, names: &Names) -> bool {
        if self
            .detect_imports
            .iter()
            .any(|m| a.imports.modules.iter().any(|seen| seen == m))
        {
            return true;
        }
        // dbt injects `dbt` and `session`; nothing is imported.
        a.functions.iter().any(|f| self.owns_signature(f, names))
    }

    /// Does the framework control this function's signature?
    pub fn owns_signature(&self, f: &FunctionDef, names: &Names) -> bool {
        if f.decorators
            .iter()
            .any(|d| self.provides(d, self.owned_decorators, names))
        {
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
    pub fn governs_signature(&self, f: &FunctionDef, names: &Names) -> bool {
        self.governing_decorator(f, names).is_some()
    }

    /// The decorator, as written, by which this framework governs `f`.
    pub fn governing_decorator<'f>(&self, f: &'f FunctionDef, names: &Names) -> Option<&'f str> {
        f.decorators
            .iter()
            .find(|d| self.provides(d, self.governed_decorators, names))
            .map(String::as_str)
    }

    /// Is `d`, a decorator as written, one of `listed`, and does it come from
    /// this framework?
    ///
    /// By name, `listed` matches the full dotted path or its final segment, so
    /// `task` matches `@task` and `@airflow.decorators.task`. That alone let
    /// `@table` match Flask's `@app.view`-style decorators and locally defined
    /// ones, and an owned decorator exempts its function from KV001 silently.
    /// So the decorator's root name must also come from the framework:
    ///
    /// - imported from a module this profile detects (`from dlt import table`,
    ///   or `import pyspark.sql.functions as F` for `@F.pandas_udf`): matches;
    /// - imported from anywhere else, or defined in the file: no match;
    /// - bound nowhere in the file: matches by name alone, as before. Such
    ///   code cannot run, short of a star import.
    fn provides(&self, d: &str, listed: &[&str], names: &Names) -> bool {
        let by_name = listed
            .iter()
            .any(|want| d == *want || d.rsplit('.').next().is_some_and(|last| last == *want));
        if !by_name {
            return false;
        }
        let root = d.split('.').next().unwrap_or(d);
        match names.origin(root) {
            NameOrigin::Imported(modules) => modules.iter().any(|m| self.is_own_module(m)),
            NameOrigin::Defined => false,
            NameOrigin::Unbound => true,
        }
    }

    /// Is `module` one this profile detects, by its root (`pyspark` for
    /// `pyspark.sql.functions`)? A relative import never is.
    fn is_own_module(&self, module: &str) -> bool {
        !module.starts_with('.')
            && self
                .detect_imports
                .contains(&module.split('.').next().unwrap_or(""))
    }
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

    fn detected(profile: &str, src: &str) -> bool {
        let a = analyze(src);
        by_name(profile).unwrap().detected(&a, &Names::of(&a))
    }

    /// Does `profile` own the signature of the last function in `src`?
    fn owns(profile: &str, src: &str) -> bool {
        let a = analyze(src);
        let f = a.functions.last().expect("a function");
        by_name(profile).unwrap().owns_signature(f, &Names::of(&a))
    }

    /// Does `profile` govern the last function in `src`?
    fn governs(profile: &str, src: &str) -> bool {
        let a = analyze(src);
        let f = a.functions.last().expect("a function");
        by_name(profile)
            .unwrap()
            .governs_signature(f, &Names::of(&a))
    }

    #[test]
    fn detects_by_import() {
        assert!(detected("polars", "import polars as pl\n"));
        assert!(!detected("flink", "import polars as pl\n"));
        assert!(detected(
            "snowpark",
            "from snowflake.snowpark import Session\n"
        ));
    }

    #[test]
    fn detects_dbt_by_signature_without_any_import() {
        let src = "def model(dbt, session):\n    return session.table(\"x\")\n";
        assert!(detected("dbt", src));
        assert!(owns("dbt", src));
    }

    #[test]
    fn dbt_signature_must_match_exactly() {
        // A user function that merely happens to be called `model`.
        assert!(!owns("dbt", "def model(x, y):\n    pass\n"));
    }

    #[test]
    fn owned_decorators_match_bare_and_dotted() {
        assert!(owns(
            "databricks",
            "import dlt\n@dlt.table\ndef daily_job():\n    pass\n"
        ));
        assert!(owns(
            "databricks",
            "from dlt import table\n@table\ndef daily_job():\n    pass\n"
        ));
        assert!(owns(
            "flink",
            "from pyflink.table import DataTypes, udf\n\
             @udf(result_type=DataTypes.INT())\ndef run_calc(x):\n    pass\n"
        ));
    }

    /// Every Delta Live Tables expectation decorator, on its own, makes the
    /// function framework-owned - not only those that also carry `@dlt.table`.
    #[test]
    fn every_dlt_expectation_is_owned() {
        for expectation in [
            "expect(\"ok\", \"x > 0\")",
            "expect_all({\"ok\": \"x > 0\"})",
            "expect_or_drop(\"ok\", \"x > 0\")",
            "expect_or_fail(\"ok\", \"x > 0\")",
            "expect_all_or_drop({\"ok\": \"x > 0\"})",
            "expect_all_or_fail({\"ok\": \"x > 0\"})",
        ] {
            let src = format!("import dlt\n@dlt.{expectation}\ndef orders_pipeline():\n    pass\n");
            assert!(owns("databricks", &src), "@dlt.{expectation}");
        }
    }

    /// The root of a dotted decorator resolves through an alias.
    #[test]
    fn an_aliased_module_resolves() {
        assert!(owns(
            "databricks",
            "import pyspark.sql.functions as F\n@F.pandas_udf(\"long\")\ndef run_x(s):\n    return s\n"
        ));
        assert!(owns(
            "databricks",
            "from pyspark.sql import functions as F\n@F.pandas_udf(\"long\")\ndef run_x(s):\n    return s\n"
        ));
        assert!(owns(
            "databricks",
            "import dlt as d\n@d.view\ndef run_x():\n    pass\n"
        ));
    }

    /// A decorator that only shares an owned decorator's name exempts
    /// nothing: imported from elsewhere, or defined in the file.
    #[test]
    fn a_name_from_elsewhere_or_defined_here_is_not_owned() {
        // Flask: `app` is assigned in the file.
        assert!(!owns(
            "databricks",
            "from flask import Flask\nimport pyspark\napp = Flask(__name__)\n\
             @app.view\ndef run_x():\n    pass\n"
        ));
        // A local decorator called `table`.
        assert!(!owns(
            "databricks",
            "import pyspark\ndef table(f):\n    return f\n@table\ndef run_x():\n    pass\n"
        ));
        // `table` imported, but not from a module the profile detects.
        assert!(!owns(
            "databricks",
            "import pyspark\nfrom mylib.decorators import table\n@table\ndef run_x():\n    pass\n"
        ));
        // A relative import is never the framework's.
        assert!(!owns(
            "databricks",
            "import pyspark\nfrom .decorators import view\n@view\ndef run_x():\n    pass\n"
        ));
        // Snowpark's `udf`, imported from PyFlink: Flink owns it, Snowpark not.
        let src =
            "import snowflake\nfrom pyflink.table import udf\n@udf\ndef run_x(a):\n    return a\n";
        assert!(!owns("snowpark", src));
        assert!(owns("flink", src));
    }

    /// An import wins over a fallback definition of the same name.
    #[test]
    fn an_import_wins_over_a_fallback_definition() {
        assert!(owns(
            "databricks",
            "try:\n    from dlt import table\nexcept ImportError:\n    table = None\n\
             @table\ndef run_x():\n    pass\n"
        ));
    }

    /// A name bound nowhere in the file keeps the old match by name.
    #[test]
    fn an_unbound_name_still_matches_by_name() {
        assert!(owns("databricks", "@table\ndef run_x():\n    pass\n"));
        assert!(owns(
            "databricks",
            "from dlt import *\n@view\ndef run_x():\n    pass\n"
        ));
    }

    /// Airflow's governed `@task` follows the same rule.
    #[test]
    fn governed_decorators_resolve_the_same_way() {
        assert!(governs(
            "airflow",
            "from airflow.decorators import task\n@task\ndef extract():\n    pass\n"
        ));
        assert!(governs(
            "airflow",
            "import airflow.decorators as ad\n@ad.task\ndef extract():\n    pass\n"
        ));
        assert!(!governs(
            "airflow",
            "import airflow\nfrom celery import shared_task as task\n@task\ndef extract():\n    pass\n"
        ));
        assert!(!governs(
            "airflow",
            "import airflow\napp = make_app()\n@app.task\ndef extract():\n    pass\n"
        ));
        assert!(governs("airflow", "@task\ndef extract():\n    pass\n"));
    }

    /// Detection by owned decorator obeys the rule too: Flask's `@app.view`
    /// does not make a file a Databricks file.
    #[test]
    fn a_borrowed_decorator_name_does_not_detect_a_framework() {
        assert!(!detected(
            "databricks",
            "from flask import Flask\napp = Flask(__name__)\n@app.view\ndef index():\n    pass\n"
        ));
    }

    #[test]
    fn databricks_pat_pattern() {
        let re = regex::Regex::new(by_name("databricks").unwrap().secret_values[0].1).unwrap();
        assert!(re.is_match("dapi1234567890abcdef1234567890abcdef"));
        assert!(!re.is_match("dapi-not-a-token"));
    }
}
