//! `kv-cli mcp` driven over stdio, as an MCP client drives it: protocol
//! hygiene (stdout carries only JSON-RPC, no colour, no star line, a clean
//! exit) and a full session with networking disabled where the platform
//! allows it. The protocol and security details are tested in-crate
//! (`src/mcp/tests.rs`), where the flagged literals are at hand.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

mod common;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_kv-cli")
}

/// A scratch copy of the samples the tools run over, with a defaults contract.
fn workspace(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("kvcli-mcp-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("configs")).unwrap();
    let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
    for f in ["deploy.py", "credentials.py"] {
        std::fs::copy(samples.join(f), root.join(f)).unwrap();
    }
    for entry in std::fs::read_dir(samples.join("configs")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), root.join("configs").join(entry.file_name())).unwrap();
    }
    std::fs::write(root.join(".kovallent.yaml"), "version: 1\n").unwrap();
    root
}

/// A running `kv-cli mcp`, read a line at a time.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    /// Every line the server wrote to stdout.
    lines: Vec<String>,
    next: i64,
}

impl Client {
    fn start(dir: &Path, wrapper: &[&str]) -> Self {
        let mut cmd = match wrapper.split_first() {
            Some((program, args)) => {
                let mut c = Command::new(program);
                c.args(args).arg(bin());
                c
            }
            None => Command::new(bin()),
        };
        let mut child = cmd
            .arg("mcp")
            .current_dir(dir)
            // Colour forced on: the server must still emit none.
            .env("CLICOLOR_FORCE", "1")
            .env_remove("NO_COLOR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("kv-cli mcp starts");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            lines: Vec::new(),
            next: 0,
        }
    }

    fn write_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("a line from the server");
        assert!(!line.is_empty(), "the server closed stdout early");
        self.lines.push(line.trim_end().to_string());
        serde_json::from_str(&line).expect("stdout carries JSON")
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": self.next, "method": method, "params": params });
        self.write_line(&msg.to_string());
        let r = self.read();
        assert_eq!(r["id"], json!(self.next), "{r}");
        r
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let r = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        );
        assert!(r.get("error").is_none(), "{tool}: {r}");
        r["result"].clone()
    }

    /// Close stdin and wait for the server to exit; its status and stderr.
    fn finish(mut self) -> (Option<i32>, String, Vec<String>) {
        drop(self.stdin.take());
        // Whatever is left on stdout, to the end.
        for line in (&mut self.stdout).lines() {
            self.lines.push(line.unwrap());
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                break s;
            }
            assert!(
                Instant::now() < deadline,
                "the server didn't exit after stdin closed"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut stderr = String::new();
        std::io::Read::read_to_string(&mut self.child.stderr.take().unwrap(), &mut stderr).unwrap();
        (status.code(), stderr, self.lines)
    }
}

/// A legacy session (as clients released before 2026-07-28 open one) that
/// calls every tool, including a fix preview and its apply, with malformed
/// lines in between. Returns what the session saw.
fn full_session(client: &mut Client) -> Vec<Value> {
    let mut results = Vec::new();
    let init = client.request(
        "initialize",
        json!({ "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": { "name": "kv-cli-e2e", "version": "0" } }),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    client.write_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    let tools = client.request("tools/list", json!({}));
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 7);

    // Malformed lines get errors, and the server carries on.
    for (line, code) in [
        ("this is not json", -32700),
        ("[1, 2]", -32600),
        ("{\"jsonrpc\": \"2.0\"", -32700),
    ] {
        client.write_line(line);
        assert_eq!(client.read()["error"]["code"], code, "{line}");
    }
    client.write_line(r#"{"jsonrpc":"2.0","id":"x","method":"no/such/method"}"#);
    assert_eq!(client.read()["error"]["code"], -32601);

    let audit = client.call("audit", json!({ "paths": ["deploy.py"] }));
    assert_eq!(audit["isError"], false);
    assert_eq!(audit["structuredContent"]["verdict"], "FAIL");
    common::check_payload(&audit["structuredContent"]["payload"]);
    results.push(audit);
    for (tool, args) in [
        ("audit", json!({})),
        ("explain", json!({ "code": "KV002" })),
        ("contract_show", json!({})),
        ("contract_check", json!({})),
        ("frameworks", json!({})),
        ("fix_preview", json!({ "paths": ["configs"] })),
    ] {
        results.push(client.call(tool, args));
    }
    let preview = client.call(
        "fix_preview",
        json!({ "paths": ["deploy.py", "credentials.py"] }),
    );
    let id = preview["structuredContent"]["preview_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(preview["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("--- a/deploy.py"));
    results.push(preview);
    let applied = client.call("fix_apply", json!({ "preview_id": id }));
    assert_eq!(applied["isError"], false, "{applied}");
    results.push(applied);
    let again = client.call("fix_apply", json!({ "preview_id": id }));
    assert_eq!(again["isError"], true, "a preview is used once");
    results.push(again);
    results
}

/// Every line on stdout is one JSON-RPC message, with no colour codes, and
/// nothing else - not the star line - appears on any channel.
fn check_hygiene(status: Option<i32>, stderr: &str, lines: &[String]) {
    assert_eq!(status, Some(0), "a clean exit; stderr: {stderr}");
    assert!(!lines.is_empty());
    for line in lines {
        assert!(!line.contains('\u{1b}'), "a colour code on stdout: {line}");
        let m: Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"));
        assert_eq!(m["jsonrpc"], "2.0", "{line}");
        let response =
            m.get("id").is_some() && (m.get("result").is_some() ^ m.get("error").is_some());
        let request = m["method"].is_string();
        assert!(response || request, "not a JSON-RPC message: {line}");
    }
    assert!(!stderr.contains('\u{1b}'), "{stderr}");
    assert!(
        !stderr.contains("star"),
        "the star line in mcp mode: {stderr}"
    );
}

#[test]
fn a_full_session_writes_only_json_rpc_to_stdout() {
    let dir = workspace("hygiene");
    let mut client = Client::start(&dir, &[]);
    full_session(&mut client);
    let (status, stderr, lines) = client.finish();
    check_hygiene(status, &stderr, &lines);
    assert!(
        stderr.contains("kv-cli mcp"),
        "a startup line on stderr: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_server_exits_cleanly_when_stdin_closes() {
    let dir = workspace("eof");
    let (status, stderr, lines) = Client::start(&dir, &[]).finish();
    assert_eq!(status, Some(0), "{stderr}");
    assert!(lines.is_empty(), "nothing on stdout without a request");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_modern_client_needs_no_handshake() {
    let dir = workspace("modern");
    let mut client = Client::start(&dir, &[]);
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    let d = client.request("server/discover", json!({ "_meta": meta }));
    assert_eq!(
        d["result"]["supportedVersions"],
        json!(["2026-07-28", "2025-11-25"])
    );
    let r = client.request(
        "tools/call",
        json!({ "_meta": meta, "name": "audit", "arguments": { "paths": ["deploy.py"] } }),
    );
    assert_eq!(r["result"]["resultType"], "complete");
    assert_eq!(r["result"]["structuredContent"]["verdict"], "FAIL");
    let (status, stderr, lines) = client.finish();
    check_hygiene(status, &stderr, &lines);
    let _ = std::fs::remove_dir_all(&dir);
}

/// How to run a command with no network on this machine, if there is a way:
/// a new network namespace holds only a loopback interface, which is down.
fn without_network() -> Option<Vec<&'static str>> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    for wrapper in [
        vec!["unshare", "--net"],
        vec!["unshare", "--net", "--map-root-user"],
    ] {
        let ok = Command::new(wrapper[0])
            .args(&wrapper[1..])
            .arg("true")
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return Some(wrapper);
        }
    }
    None
}

#[test]
fn the_full_session_runs_with_networking_disabled() {
    let Some(wrapper) = without_network() else {
        eprintln!(
            "skipped: this platform can't run a process without a network (no unshare --net)"
        );
        return;
    };
    let dir = workspace("offline");
    let mut client = Client::start(&dir, &wrapper);
    let offline = full_session(&mut client);
    let (status, stderr, lines) = client.finish();
    check_hygiene(status, &stderr, &lines);

    // The same session with the network, from a fresh copy, gives the same
    // results: the server never needed it.
    let dir2 = workspace("online");
    let mut client = Client::start(&dir2, &[]);
    let online = full_session(&mut client);
    client.finish();
    // Run-specific values - the time, preview ids - are blanked first.
    fn blank(v: &mut Value) {
        match v {
            Value::String(s) => {
                while let Some(at) = s.find("kvp_") {
                    let end = s[at + 4..]
                        .find(|c: char| !c.is_ascii_hexdigit())
                        .map_or(s.len(), |n| at + 4 + n);
                    s.replace_range(at..end, "<preview>");
                }
                if s.contains("\"timestamp\"") {
                    *s = s.lines().filter(|l| !l.contains("\"timestamp\"")).collect();
                }
            }
            Value::Array(items) => items.iter_mut().for_each(blank),
            Value::Object(map) => {
                if map.contains_key("timestamp") {
                    map.insert("timestamp".into(), Value::Null);
                }
                map.values_mut().for_each(blank);
            }
            _ => {}
        }
    }
    let strip = |mut v: Value| {
        blank(&mut v);
        v
    };
    let offline: Vec<Value> = offline.into_iter().map(strip).collect();
    let online: Vec<Value> = online.into_iter().map(strip).collect();
    assert_eq!(offline.len(), online.len());
    for (a, b) in offline.iter().zip(&online) {
        assert_eq!(a, b);
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

#[test]
fn help_points_at_the_mcp_docs() {
    let out = Command::new(bin()).arg("--help").output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("kv-cli mcp"), "{help}");
    assert!(help.contains("docs/mcp.md"), "{help}");
    let out = Command::new(bin())
        .args(["mcp", "--help"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("docs/mcp.md"));
}
