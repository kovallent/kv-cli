//! In-crate tests for the MCP server: the protocol, both eras, roots, and
//! the security properties ADR 0006 requires. They drive `Server::handle`
//! directly, so they can read the flagged literals from the same audit the
//! tools run; `tests/mcp.rs` drives the binary over stdio.

use super::preview::PREVIEW_TTL;
use super::workspace::Workspace;
use super::*;
use crate::audit::FindingKind;
use std::path::{Path, PathBuf};

const REPO: &str = env!("CARGO_MANIFEST_DIR");

/// A scratch workspace holding the samples the security tests cover, and a
/// defaults contract so nothing above it applies.
fn fixture(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("kvcli-mcp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let samples = Path::new(REPO).join("samples");
    std::fs::copy(samples.join("credentials.py"), root.join("credentials.py")).unwrap();
    std::fs::copy(samples.join("deploy.py"), root.join("deploy.py")).unwrap();
    copy_dir(&samples.join("configs"), &root.join("configs"));
    std::fs::write(root.join(".kovallent.yaml"), "version: 1\n").unwrap();
    root.canonicalize().unwrap()
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), to).unwrap();
        }
    }
}

fn meta() -> Value {
    json!({
        VERSION_KEY: MODERN,
        CAPABILITIES_KEY: {},
        "io.modelcontextprotocol/clientInfo": { "name": "kv-cli-tests", "version": "0" },
    })
}

/// A client session against an in-process server, recording every message
/// the server sends.
struct Session {
    server: Server,
    sent: Vec<Value>,
    next: i64,
    modern: bool,
}

impl Session {
    fn modern(root: &Path) -> Self {
        Self::with(
            Server::new(Workspace::rooted_at(root.to_path_buf()), PREVIEW_TTL),
            true,
        )
    }

    fn with(server: Server, modern: bool) -> Self {
        Self {
            server,
            sent: Vec::new(),
            next: 0,
            modern,
        }
    }

    fn legacy(root: &Path, capabilities: Value) -> Self {
        let mut s = Self::with(
            Server::new(Workspace::rooted_at(root.to_path_buf()), PREVIEW_TTL),
            false,
        );
        let init = s.request(
            "initialize",
            json!({
                "protocolVersion": LEGACY,
                "capabilities": capabilities,
                "clientInfo": { "name": "kv-cli-tests", "version": "0" },
            }),
        );
        assert_eq!(init["result"]["protocolVersion"], LEGACY);
        s
    }

    /// Send a raw message; return what the server sent back.
    fn send(&mut self, msg: Value) -> Vec<Value> {
        let out = self.server.handle(msg);
        assert!(out.deferred.is_none(), "unexpectedly deferred");
        self.sent.extend(out.send.iter().cloned());
        out.send
    }

    /// A request, with `_meta` when the session is modern. Returns the
    /// response.
    fn request(&mut self, method: &str, mut params: Value) -> Value {
        self.next += 1;
        if self.modern {
            params["_meta"] = meta();
        }
        let id = self.next;
        let out =
            self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        out.into_iter()
            .find(|m| m["id"] == json!(id))
            .expect("a response to the request")
    }

    /// A tool call's result object.
    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let r = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        );
        assert!(r.get("error").is_none(), "{tool}: protocol error {r}");
        r["result"].clone()
    }
}

fn path(p: &Path) -> String {
    p.display().to_string()
}

// --- the security tests ADR 0006 and the build require ----------------------

/// Every literal a KV002 finding flags in the samples, from the same audit
/// the `audit` tool runs: each value, and each part of a split one. Read from
/// the findings themselves, so the list can't drift from the detectors.
fn flagged_literals(root: &Path) -> Vec<String> {
    let paths = [
        root.join("credentials.py"),
        root.join("deploy.py"),
        root.join("configs"),
    ];
    let run = crate::run_audit(&paths, None, false, None).expect("the samples audit");
    let mut out: Vec<String> = run
        .findings
        .iter()
        .filter_map(|f| match &f.kind {
            FindingKind::HardcodedSecret { flagged, .. } => Some(flagged),
            _ => None,
        })
        .flat_map(|f| {
            std::iter::once(f.text.clone()).chain(f.parts.iter().filter(|p| p.len() >= 8).cloned())
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Assert that no string in any message contains a literal. A string that is
/// a rule page verbatim - what `explain` returns - is skipped, as the SARIF
/// test skips the rules' `help`: `docs/rules/KV002.md` happens to use one of
/// the samples' example values, `abcd1234efgh`, to show a suppressed line.
fn assert_no_literal(messages: &[Value], literals: &[String]) {
    fn walk(v: &Value, literals: &[String], at: &str) {
        match v {
            Value::String(s) => {
                if crate::RULES.iter().any(|(_, page)| s == page) {
                    return;
                }
                for lit in literals {
                    assert!(
                        !s.contains(lit.as_str()),
                        "{at} leaked a flagged literal ({} chars, starting {:?})",
                        lit.chars().count(),
                        lit.chars().take(2).collect::<String>()
                    );
                }
            }
            Value::Array(items) => items.iter().for_each(|i| walk(i, literals, at)),
            Value::Object(map) => map.values().for_each(|i| walk(i, literals, at)),
            _ => {}
        }
    }
    for m in messages {
        walk(m, literals, &format!("response {}", m["id"]));
    }
}

/// Run every tool over `root` - including a fix preview and its apply - and
/// return the session.
fn run_every_tool(root: &Path) -> Session {
    let mut s = Session::modern(root);
    for p in ["credentials.py", "deploy.py", "configs"] {
        let r = s.call("audit", json!({ "paths": [path(&root.join(p))] }));
        assert_eq!(r["isError"], false, "{p}: {r}");
    }
    s.call("audit", json!({}));
    s.call("audit", json!({ "strict": true }));
    s.call("explain", json!({ "code": "KV002" }));
    s.call("contract_show", json!({}));
    s.call("contract_check", json!({}));
    s.call("frameworks", json!({}));
    for p in ["configs", "."] {
        s.call("fix_preview", json!({ "paths": [path(&root.join(p))] }));
    }
    let preview = s.call(
        "fix_preview",
        json!({ "paths": [path(&root.join("credentials.py")), path(&root.join("deploy.py"))] }),
    );
    let id = preview["structuredContent"]["preview_id"]
        .as_str()
        .expect("a preview id")
        .to_string();
    let applied = s.call("fix_apply", json!({ "preview_id": id }));
    assert_eq!(applied["isError"], false, "{applied}");
    // And once more after the rewrite.
    s.call("audit", json!({}));
    s.call("fix_preview", json!({ "paths": [path(root)] }));
    s
}

#[test]
fn no_result_contains_a_flagged_literal() {
    let root = fixture("secrets");
    let literals = flagged_literals(&root);
    // The audit found what the samples hold, split token parts included -
    // otherwise this test would pass by checking nothing.
    assert!(literals.len() >= 8, "only {} literals", literals.len());
    assert!(literals
        .iter()
        .any(|l| l.len() == 20 && l.starts_with("s3")));
    assert!(
        literals.iter().any(|l| l.starts_with("gh") && l.len() < 41),
        "a split token's parts"
    );

    let s = run_every_tool(&root);
    assert!(s.sent.len() >= 15);
    assert_no_literal(&s.sent, &literals);
    // The removed lines of the preview still say what was there, redacted.
    let previews: Vec<&Value> = s
        .sent
        .iter()
        .filter(|m| m.to_string().contains("--- a/"))
        .collect();
    assert!(previews
        .iter()
        .any(|m| m.to_string().contains("(<redacted>, 20 chars)")));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_contract_is_never_written() {
    let root = fixture("contract");
    let contract = root.join(".kovallent.yaml");
    let before = std::fs::read(&contract).unwrap();
    run_every_tool(&root);
    assert_eq!(std::fs::read(&contract).unwrap(), before, "byte-identical");
    assert!(!root.join(".kovallent.yaml.kvbak").exists());
    // Sanity: the run did write, elsewhere.
    assert!(root.join("deploy.py.kvbak").exists());
    let _ = std::fs::remove_dir_all(&root);
}

/// Every tool that takes a path refuses one outside the workspace, however
/// it is spelled.
#[test]
fn no_path_escapes_the_workspace() {
    let root = fixture("escape");
    let outside = root
        .parent()
        .unwrap()
        .join(format!("kvcli-mcp-outside-{}", std::process::id()));
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        outside.join("leak.py"),
        "password = \"s3cr3t-outside-pw\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.join("leak.py"), root.join("link.py")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("linkdir")).unwrap();
    }
    let mut escapes = vec![
        "../".to_string(),
        format!(
            "../{}/leak.py",
            outside.file_name().unwrap().to_string_lossy()
        ),
        "configs/../../".to_string(),
        path(&outside.join("leak.py")),
        path(&outside),
        "/".to_string(),
    ];
    if cfg!(unix) {
        escapes.extend([
            "link.py".to_string(),
            "linkdir".to_string(),
            "linkdir/leak.py".to_string(),
        ]);
    }
    let mut s = Session::modern(&root);
    for tool in ["audit", "fix_preview"] {
        for arg in &escapes {
            let r = s.call(tool, json!({ "paths": [arg] }));
            assert_eq!(r["isError"], true, "{tool} {arg}: {r}");
            let text = r["content"][0]["text"].as_str().unwrap();
            assert!(
                text.contains("outside the workspace"),
                "{tool} {arg}: {text}"
            );
        }
    }
    // A link found by walking a folder isn't followed out of the project.
    let r = s.call("audit", json!({ "paths": [path(&root)] }));
    assert!(!r.to_string().contains("leak.py"), "{r}");
    let r = s.call("fix_preview", json!({ "paths": [path(&root)] }));
    assert!(!r.to_string().contains("leak.py"), "{r}");
    assert_no_literal(&s.sent, &["s3cr3t-outside-pw".to_string()]);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
}

fn preview_of(s: &mut Session, file: &Path) -> String {
    let r = s.call("fix_preview", json!({ "paths": [path(file)] }));
    r["structuredContent"]["preview_id"]
        .as_str()
        .expect("a preview id")
        .to_string()
}

fn refusal(r: &Value) -> String {
    assert_eq!(r["isError"], true, "{r}");
    r["content"][0]["text"].as_str().unwrap().to_string()
}

#[test]
fn apply_without_a_valid_preview_is_refused() {
    let root = fixture("apply");
    let file = root.join("deploy.py");
    let original = std::fs::read(&file).unwrap();
    let mut s = Session::modern(&root);

    // No preview_id at all.
    assert!(refusal(&s.call("fix_apply", json!({}))).contains("preview_id"));
    // One that never existed.
    assert!(
        refusal(&s.call("fix_apply", json!({ "preview_id": "kvp_nope" })))
            .contains("fix_preview again")
    );
    // Anything else is an invalid argument too.
    assert!(refusal(&s.call(
        "fix_apply",
        json!({ "preview_id": "x", "paths": ["deploy.py"] })
    ))
    .contains("invalid arguments"));

    // A file edited after the preview.
    let id = preview_of(&mut s, &file);
    let mut edited = original.clone();
    edited.extend_from_slice(b"\n# edited after the preview\n");
    std::fs::write(&file, &edited).unwrap();
    let why = refusal(&s.call("fix_apply", json!({ "preview_id": id })));
    assert!(
        why.contains("has changed since the preview") && why.contains("fix_preview again"),
        "{why}"
    );
    assert_eq!(std::fs::read(&file).unwrap(), edited, "nothing written");
    assert!(!root.join("deploy.py.kvbak").exists());

    // The contract changed after the preview.
    std::fs::write(&file, &original).unwrap();
    let id = preview_of(&mut s, &file);
    let contract = root.join(".kovallent.yaml");
    std::fs::write(&contract, "version: 1\nsecrets:\n  detectors: 2\n").unwrap();
    let why = refusal(&s.call("fix_apply", json!({ "preview_id": id })));
    assert!(why.contains("contract has changed"), "{why}");
    std::fs::write(&contract, "version: 1\n").unwrap();

    // Used once, it is gone.
    let id = preview_of(&mut s, &file);
    assert_eq!(
        s.call("fix_apply", json!({ "preview_id": id }))["isError"],
        false
    );
    assert!(refusal(&s.call("fix_apply", json!({ "preview_id": id }))).contains("already used"));

    // An expired one.
    std::fs::write(&file, &original).unwrap();
    let mut s = Session::with(
        Server::new(Workspace::rooted_at(root.clone()), Duration::ZERO),
        true,
    );
    let id = preview_of(&mut s, &file);
    std::thread::sleep(Duration::from_millis(5));
    assert!(refusal(&s.call("fix_apply", json!({ "preview_id": id }))).contains("expired"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_plan_that_touches_the_contract_is_refused() {
    let root = fixture("planned");
    let ws = Workspace::rooted_at(root.clone());
    let e = tools::check_planned(&ws, &root.join(".kovallent.yaml")).unwrap_err();
    assert!(e.contains("no tool changes the contract"), "{e}");
    assert!(tools::check_planned(&ws, &root.join("deploy.py")).is_ok());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn apply_writes_exactly_the_preview_with_backups_like_fix() {
    let root = fixture("exact");
    let mut s = Session::modern(&root);
    let preview = s.call(
        "fix_preview",
        json!({ "paths": [path(&root.join("deploy.py"))] }),
    );
    let id = preview["structuredContent"]["preview_id"]
        .as_str()
        .unwrap()
        .to_string();
    let untouched = std::fs::read(root.join("credentials.py")).unwrap();
    let applied = s.call("fix_apply", json!({ "preview_id": id }));
    assert_eq!(applied["isError"], false, "{applied}");
    let st = &applied["structuredContent"];
    assert_eq!(st["applied"].as_array().unwrap().len(), 1);
    assert_eq!(st["backups"].as_array().unwrap().len(), 1);
    assert!(st["warnings"].to_string().contains("rotate"));
    // The backup is the original; the other sample is untouched.
    assert_eq!(
        std::fs::read(root.join("deploy.py.kvbak")).unwrap(),
        std::fs::read(Path::new(REPO).join("samples/deploy.py")).unwrap()
    );
    assert_eq!(
        std::fs::read(root.join("credentials.py")).unwrap(),
        untouched
    );
    // What `kv-cli fix` writes, byte for byte.
    let cli = fixture("exact-cli");
    let (_, plan) = crate::plan_fix(&[cli.join("deploy.py")], None).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("deploy.py")).unwrap(),
        plan.changes[0].new
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&cli);
}

// --- tool results -------------------------------------------------------------

#[test]
fn audit_verdicts_are_kv_clis_own() {
    let root = fixture("verdicts");
    std::fs::write(root.join("clean.py"), "def helper(x):\n    return x\n").unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    let mut s = Session::modern(&root);

    let pass = s.call("audit", json!({ "paths": [path(&root.join("clean.py"))] }));
    assert_eq!(pass["isError"], false);
    assert_eq!(pass["structuredContent"]["verdict"], "PASS");
    assert_eq!(pass["structuredContent"]["exit_code"], 0);

    let fail = s.call("audit", json!({ "paths": [path(&root.join("deploy.py"))] }));
    assert_eq!(fail["isError"], false, "findings are a normal result");
    assert_eq!(fail["structuredContent"]["verdict"], "FAIL");
    assert_eq!(fail["structuredContent"]["exit_code"], 1);
    let run = &fail["structuredContent"]["payload"]["run"];
    assert_eq!(run["schema_version"], 1);
    assert_eq!(run["contract_expected"], Value::Null);
    assert_eq!(run["contract_drift"], false);
    assert_eq!(run["contract_sha256"], "a6edcff00199c1cf");

    let error = s.call("audit", json!({ "paths": [path(&root.join("empty"))] }));
    assert_eq!(error["isError"], true, "the check couldn't run");
    assert_eq!(error["structuredContent"]["verdict"], "TOOL ERROR");
    assert_eq!(error["structuredContent"]["exit_code"], 2);

    // No expect_contract argument: the pin is CI's.
    let r = s.call("audit", json!({ "expect_contract": "a6edcff00199c1cf" }));
    assert!(refusal(&r).contains("invalid arguments"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_payload_is_the_clis_unchanged() {
    let root = fixture("payload");
    let mut s = Session::modern(&root);
    let r = s.call("audit", json!({ "paths": [path(&root.join("deploy.py"))] }));
    let mut tool = r["structuredContent"]["payload"].clone();
    let run = crate::run_audit(&[root.join("deploy.py")], None, false, None).unwrap();
    let identity = crate::payload::IdentityFlags {
        repo: None,
        commit: None,
        branch: None,
    };
    let report = run.report(false, &identity);
    let mut cli = serde_json::to_value(crate::build_payload(&run.findings, &report)).unwrap();
    tool["run"]["timestamp"] = json!("");
    cli["run"]["timestamp"] = json!("");
    assert_eq!(tool, cli);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_other_tools_return_their_commands_output() {
    let root = fixture("commands");
    let mut s = Session::modern(&root);
    let r = s.call("explain", json!({ "code": "kv003" }));
    assert_eq!(
        r["content"][0]["text"],
        crate::rule_reference("KV003").unwrap()
    );
    assert!(refusal(&s.call("explain", json!({ "code": "KV999" }))).contains("unknown rule"));
    assert!(refusal(&s.call("explain", json!({}))).contains("invalid arguments"));

    let r = s.call("contract_show", json!({}));
    assert_eq!(r["isError"], false);
    assert!(r["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("# fingerprint a6edcff00199c1cf"));
    let r = s.call("contract_check", json!({}));
    assert!(r["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("is valid"));
    let r = s.call("frameworks", json!({}));
    assert_eq!(r["content"][0]["text"], crate::frameworks_output());
    assert!(refusal(&s.call("frameworks", json!({ "x": 1 }))).contains("invalid arguments"));

    // An invalid contract is exit 2: a tool error, with the problems.
    std::fs::write(
        root.join(".kovallent.yaml"),
        "version: 1\nsecrets:\n  detectors: 3\n",
    )
    .unwrap();
    let r = s.call("contract_check", json!({}));
    assert_eq!(r["isError"], true);
    assert!(r["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains(":3: secrets.detectors"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_tool_list_and_its_annotations() {
    let tools = tools::definitions();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "audit",
            "explain",
            "contract_show",
            "contract_check",
            "frameworks",
            "fix_preview",
            "fix_apply"
        ]
    );
    for t in &tools {
        let a = &t["annotations"];
        let writes = t["name"] == "fix_apply";
        assert_eq!(a["readOnlyHint"], !writes, "{}", t["name"]);
        assert_eq!(a["destructiveHint"], writes, "{}", t["name"]);
        assert_eq!(a["idempotentHint"], true, "{}", t["name"]);
        assert_eq!(a["openWorldHint"], false, "{}", t["name"]);
        assert_eq!(t["inputSchema"]["type"], "object");
        assert_eq!(
            t["inputSchema"]["additionalProperties"], false,
            "{}",
            t["name"]
        );
    }
    let audit = tools[0]["description"].as_str().unwrap();
    assert!(audit.contains("NOT the gate") && audit.contains("contract_drift"));
    assert!(!tools
        .iter()
        .any(|t| t["inputSchema"].to_string().contains("expect")));
}

// --- the protocol ---------------------------------------------------------

fn error_code(r: &Value) -> i64 {
    r["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("not an error: {r}"))
}

#[test]
fn a_modern_session_discovers_and_lists() {
    let root = fixture("modern");
    let mut s = Session::modern(&root);
    let d = s.request("server/discover", json!({}));
    assert_eq!(d["result"]["supportedVersions"], json!([MODERN, LEGACY]));
    assert_eq!(d["result"]["resultType"], "complete");
    assert_eq!(d["result"]["_meta"][SERVER_INFO_KEY]["name"], "kv-cli");
    let l = s.request("tools/list", json!({}));
    assert_eq!(l["result"]["tools"].as_array().unwrap().len(), 7);
    assert_eq!(l["result"]["cacheScope"], "public");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn version_negotiation() {
    let root = fixture("versions");
    let mut s = Session::with(
        Server::new(Workspace::rooted_at(root.clone()), PREVIEW_TTL),
        false,
    );
    // A modern version this server doesn't know.
    let r = s.request(
        "tools/list",
        json!({ "_meta": { VERSION_KEY: "2099-01-01", CAPABILITIES_KEY: {} } }),
    );
    assert_eq!(error_code(&r), -32022);
    assert_eq!(r["error"]["data"]["supported"], json!([MODERN, LEGACY]));
    assert_eq!(r["error"]["data"]["requested"], "2099-01-01");
    // No version and no handshake: malformed.
    assert_eq!(error_code(&s.request("tools/list", json!({}))), -32602);
    // The modern version without the capabilities it requires.
    let r = s.request("tools/list", json!({ "_meta": { VERSION_KEY: MODERN } }));
    assert_eq!(error_code(&r), -32602);

    // A legacy client asking for an older revision is answered 2025-11-25.
    let r = s.request(
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } }),
    );
    assert_eq!(r["result"]["protocolVersion"], LEGACY);
    assert_eq!(r["result"]["serverInfo"]["name"], "kv-cli");
    assert!(s
        .send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .is_empty());
    let l = s.request("tools/list", json!({}));
    assert_eq!(l["result"]["tools"].as_array().unwrap().len(), 7);
    assert!(l["result"].get("resultType").is_none(), "a legacy result");
    // server/discover is modern-only.
    assert_eq!(error_code(&s.request("server/discover", json!({}))), -32601);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn malformed_messages_get_json_rpc_errors() {
    let root = fixture("malformed");
    let mut s = Session::modern(&root);
    let one = |s: &mut Session, m: Value| s.send(m).pop().expect("a response");
    assert_eq!(error_code(&one(&mut s, json!([1, 2]))), -32600);
    assert_eq!(error_code(&one(&mut s, json!("hello"))), -32600);
    assert_eq!(
        error_code(&one(&mut s, json!({ "id": 1, "method": "ping" }))),
        -32600
    );
    assert_eq!(
        error_code(&one(
            &mut s,
            json!({ "jsonrpc": "2.0", "id": null, "method": "ping" })
        )),
        -32600
    );
    assert_eq!(
        error_code(&one(
            &mut s,
            json!({ "jsonrpc": "2.0", "id": 1, "method": 7 })
        )),
        -32600
    );
    assert_eq!(
        error_code(&one(
            &mut s,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "x", "params": [] })
        )),
        -32600
    );
    assert_eq!(error_code(&s.request("no/such/method", json!({}))), -32601);
    assert_eq!(
        error_code(&s.request("tools/call", json!({ "name": "init" }))),
        -32602
    );
    assert_eq!(error_code(&s.request("tools/call", json!({}))), -32602);
    assert_eq!(
        error_code(&s.request("tools/call", json!({ "name": "audit", "arguments": [] }))),
        -32602
    );
    let cancelled = Mutex::new(HashSet::new());
    let parse = s.server.handle_incoming(Incoming::Unparseable, &cancelled);
    assert_eq!(error_code(&parse.send[0]), -32700);
    // Notifications and stray responses get nothing back.
    assert!(s
        .send(json!({ "jsonrpc": "2.0", "method": "notifications/whatever" }))
        .is_empty());
    assert!(s
        .send(json!({ "jsonrpc": "2.0", "id": 99, "result": {} }))
        .is_empty());
    assert_eq!(s.request("ping", json!({}))["result"], json!({}));
    // ...and the server still answers.
    assert_eq!(
        s.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_cancelled_request_gets_no_response() {
    let root = fixture("cancel");
    let mut server = Server::new(Workspace::rooted_at(root.clone()), PREVIEW_TTL);
    let cancelled = Mutex::new(HashSet::from(["7".to_string()]));
    let msg =
        json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/list", "params": { "_meta": meta() } });
    assert!(server
        .handle_incoming(Incoming::Message(msg.clone()), &cancelled)
        .send
        .is_empty());
    // The cancellation is spent; the same id later is answered.
    assert_eq!(
        server
            .handle_incoming(Incoming::Message(msg), &cancelled)
            .send
            .len(),
        1
    );
    let _ = std::fs::remove_dir_all(&root);
}

// --- roots --------------------------------------------------------------------

#[test]
fn modern_roots_are_asked_for_and_narrow_the_workspace() {
    let root = fixture("mrtr");
    std::fs::create_dir_all(root.join("narrow")).unwrap();
    std::fs::copy(root.join("deploy.py"), root.join("narrow/deploy.py")).unwrap();
    let mut s = Session::modern(&root);
    let mut caps_meta = meta();
    caps_meta[CAPABILITIES_KEY] = json!({ "roots": {} });
    let call = |id: i64, extra: Value| {
        let mut params = json!({
            "_meta": caps_meta.clone(),
            "name": "audit",
            "arguments": { "paths": [path(&root.join("deploy.py"))] },
        });
        if let Value::Object(e) = extra {
            for (k, v) in e {
                params[k] = v;
            }
        }
        json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params })
    };
    let first = s.send(call(1, json!({}))).pop().unwrap();
    assert_eq!(first["result"]["resultType"], "input_required");
    assert_eq!(
        first["result"]["inputRequests"]["roots"]["method"],
        "roots/list"
    );
    // The retry, with a root narrower than the server's.
    let uri = format!("file://{}", root.join("narrow").display());
    let retry = s
        .send(call(
            2,
            json!({ "inputResponses": { "roots": { "roots": [{ "uri": uri }] } } }),
        ))
        .pop()
        .unwrap();
    assert_eq!(
        retry["result"]["isError"], true,
        "deploy.py is outside the declared root"
    );
    let inside = json!({ "inputResponses": { "roots": { "roots": [{ "uri": format!("file://{}", root.display()) }] } } });
    let ok = s.send(call(3, inside)).pop().unwrap();
    assert_eq!(ok["result"]["structuredContent"]["verdict"], "FAIL");
    // Tools that use no workspace don't ask.
    let explain = s.send(json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": { "_meta": caps_meta.clone(), "name": "explain", "arguments": { "code": "KV001" } } }));
    assert_eq!(explain[0]["result"]["resultType"], "complete");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn legacy_roots_are_requested_after_initialized() {
    let root = fixture("legacy-roots");
    std::fs::create_dir_all(root.join("narrow")).unwrap();
    let mut s = Session::legacy(&root, json!({ "roots": { "listChanged": true } }));
    let sent = s.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    assert_eq!(sent[0]["method"], "roots/list");
    let roots_id = sent[0]["id"].clone();
    // A tool call before the answer waits for it.
    let call = json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": { "name": "audit", "arguments": { "paths": [path(&root.join("deploy.py"))] } } });
    assert!(s.server.handle(call.clone()).deferred.is_some());
    let uri = format!("file://{}", root.join("narrow").display());
    assert!(s
        .send(json!({ "jsonrpc": "2.0", "id": roots_id, "result": { "roots": [{ "uri": uri }] } }))
        .is_empty());
    let r = s.send(call).pop().unwrap();
    assert_eq!(
        r["result"]["isError"], true,
        "outside the declared root: {r}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// --- conformance ----------------------------------------------------------

fn schema_defs(revision: &str) -> Value {
    let path = Path::new(REPO).join(format!("tests/fixtures/mcp/schema-{revision}.json"));
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    schema["$defs"].clone()
}

fn validate(defs: &Value, def: &str, instance: &Value) {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": format!("#/$defs/{def}"),
        "$defs": defs,
    });
    let v = jsonschema::validator_for(&schema).expect("the MCP schema compiles");
    let errors: Vec<String> = v
        .iter_errors(instance)
        .map(|e| e.to_string())
        .take(5)
        .collect();
    assert!(errors.is_empty(), "{def}: {errors:?}\n{instance}");
}

/// Every message the server sent, checked against the official schema of
/// the revision it was sent under.
#[test]
fn every_message_matches_the_official_schemas() {
    let root = fixture("conformance");
    let modern_defs = schema_defs(MODERN);
    let mut s = run_every_tool(&root);
    s.request("server/discover", json!({}));
    s.request("tools/list", json!({}));
    s.request(
        "tools/call",
        json!({ "name": "explain", "arguments": { "code": "KV999" } }),
    );
    s.request("no/such/method", json!({}));
    for m in &s.sent {
        validate(&modern_defs, "JSONRPCMessage", m);
        let r = &m["result"];
        if r.get("supportedVersions").is_some() {
            validate(&modern_defs, "DiscoverResult", r);
        } else if r.get("tools").is_some() {
            validate(&modern_defs, "ListToolsResult", r);
        } else if r.get("content").is_some() {
            validate(&modern_defs, "CallToolResult", r);
        } else if m.get("error").is_some() {
            validate(&modern_defs, "JSONRPCErrorResponse", m);
        }
    }
    let input_required = Session::modern(&root)
        .server
        .handle(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "_meta": { VERSION_KEY: MODERN, CAPABILITIES_KEY: { "roots": {} } },
            "name": "audit", "arguments": {} } }),
        )
        .send
        .remove(0);
    validate(
        &modern_defs,
        "InputRequiredResult",
        &input_required["result"],
    );

    let legacy_defs = schema_defs(LEGACY);
    let mut l = Session::legacy(&root, json!({}));
    l.request("tools/list", json!({}));
    l.call("audit", json!({ "paths": [path(&root.join("deploy.py"))] }));
    l.call(
        "fix_preview",
        json!({ "paths": [path(&root.join("deploy.py"))] }),
    );
    validate(&legacy_defs, "InitializeResult", &l.sent[0]["result"]);
    for m in &l.sent {
        validate(&legacy_defs, "JSONRPCMessage", m);
        if m["result"].get("tools").is_some() {
            validate(&legacy_defs, "ListToolsResult", &m["result"]);
        } else if m["result"].get("content").is_some() {
            validate(&legacy_defs, "CallToolResult", &m["result"]);
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}
