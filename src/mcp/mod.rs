//! `kv-cli mcp`: kv-cli's tools for AI coding agents, over the Model Context
//! Protocol (ADR 0006).
//!
//! JSON-RPC 2.0 over stdin and stdout, one message per line; stdout carries
//! nothing else, and diagnostics go to stderr. It speaks MCP 2026-07-28, which
//! is stateless - every request carries its protocol version in `_meta` - and,
//! for clients that open with `initialize`, the 2025-11-25 handshake: a
//! dual-era server, as that revision allows. No network, no listening
//! socket, no account.

mod diff;
mod preview;
mod tools;
mod workspace;

use preview::{PreviewStore, PREVIEW_TTL};
use serde_json::{json, Map, Value};
use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use workspace::Workspace;

/// The revision this server targets.
pub const MODERN: &str = "2026-07-28";
/// The handshake revision served to clients that open with `initialize`.
pub const LEGACY: &str = "2025-11-25";

const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";

/// How long a legacy tool call waits for the roots it asked the client for.
const ROOTS_WAIT: Duration = Duration::from_secs(5);

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const UNSUPPORTED_VERSION: i64 = -32022;

const INSTRUCTIONS: &str = "kv-cli checks Python, notebooks, config files and dbt SQL models \
against the repository's .kovallent.yaml contract. Every result is a local check, not the gate: \
whether a change passes is decided only by kv-cli's exit code in CI, against a contract \
fingerprint pinned there. No tool changes the contract, and flagged credentials are always \
redacted. To fix: fix_preview, review the diff, then fix_apply with its preview_id.";

fn server_info() -> Value {
    json!({ "name": "kv-cli", "title": "kv-cli", "version": env!("CARGO_PKG_VERSION") })
}

/// Serve until stdin closes.
pub fn run() -> Result<u8, String> {
    // Tool results carry kv-cli's own output; it must have no colour codes.
    colored::control::set_override(false);
    let workspace = Workspace::from_cwd()?;
    eprintln!(
        "kv-cli mcp {}: MCP {MODERN} (and {LEGACY}) on stdio, workspace {}",
        env!("CARGO_PKG_VERSION"),
        workspace
            .roots()
            .first()
            .map(|r| r.display().to_string())
            .unwrap_or_default()
    );

    let cancelled: Arc<Mutex<HashSet<String>>> = Arc::default();
    let (tx, rx) = mpsc::channel::<Incoming>();
    let reader_cancelled = Arc::clone(&cancelled);
    std::thread::spawn(move || read_stdin(tx, &reader_cancelled));

    let mut server = Server::new(workspace, PREVIEW_TTL);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut backlog: VecDeque<Incoming> = VecDeque::new();
    loop {
        let incoming = match backlog.pop_front() {
            Some(i) => i,
            None => match rx.recv() {
                Ok(i) => i,
                // stdin closed: the client is done.
                Err(_) => break,
            },
        };
        let mut outcome = server.handle_incoming(incoming, &cancelled);
        if let Some(deferred) = outcome.deferred.take() {
            // A legacy tool call that needs the roots just requested: wait
            // for the client's answer, holding back anything else.
            write_all(&mut out, &outcome.send)?;
            let deadline = Instant::now() + ROOTS_WAIT;
            while server.roots_pending() {
                let left = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok(Incoming::Message(m)) if is_response(&m) => {
                        let o = server.handle_incoming(Incoming::Message(m), &cancelled);
                        write_all(&mut out, &o.send)?;
                    }
                    Ok(other) => backlog.push_back(other),
                    Err(_) => {
                        eprintln!(
                            "kv-cli mcp: the client didn't answer roots/list; using the \
                             working directory as the workspace"
                        );
                        server.give_up_roots();
                    }
                }
            }
            outcome = server.handle_incoming(Incoming::Message(deferred), &cancelled);
        }
        write_all(&mut out, &outcome.send)?;
    }
    Ok(crate::EXIT_OK)
}

fn write_all(out: &mut impl Write, messages: &[Value]) -> Result<(), String> {
    for m in messages {
        let line = serde_json::to_string(m).map_err(|e| e.to_string())?;
        writeln!(out, "{line}")
            .and_then(|()| out.flush())
            .map_err(|e| format!("cannot write to stdout: {e}"))?;
    }
    Ok(())
}

/// A line from stdin.
pub enum Incoming {
    Message(Value),
    /// Not JSON at all.
    Unparseable,
}

/// Read stdin a line at a time until it closes. A cancellation is noted
/// here, as soon as it arrives, so a response still being worked on is
/// never sent.
fn read_stdin(tx: mpsc::Sender<Incoming>, cancelled: &Mutex<HashSet<String>>) {
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match lock.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let incoming = match serde_json::from_str::<Value>(line) {
            Ok(v) => {
                if v["method"] == "notifications/cancelled" {
                    if let Some(id) = v["params"].get("requestId") {
                        if let Ok(mut set) = cancelled.lock() {
                            set.insert(id.to_string());
                        }
                    }
                }
                Incoming::Message(v)
            }
            Err(_) => Incoming::Unparseable,
        };
        if tx.send(incoming).is_err() {
            return;
        }
    }
}

fn is_response(m: &Value) -> bool {
    m.get("method").is_none() && (m.get("result").is_some() || m.get("error").is_some())
}

/// What handling one message produced.
#[derive(Default)]
pub struct Outcome {
    /// Messages to write, in order.
    pub send: Vec<Value>,
    /// A request to handle again once the client has answered `roots/list`.
    pub deferred: Option<Value>,
}

impl Outcome {
    fn send(m: Value) -> Self {
        Self {
            send: vec![m],
            deferred: None,
        }
    }
}

/// Where a legacy session's roots stand.
enum Roots {
    /// The client didn't declare the roots capability: the working directory
    /// is the workspace.
    NotOffered,
    /// Asked for, with this request id.
    Pending(String),
    Known(Workspace),
}

/// A session opened with `initialize` (2025-11-25 and earlier).
struct Legacy {
    offers_roots: bool,
    roots: Roots,
}

/// Which era a request is served under.
enum Era {
    Legacy,
    /// With the request's declared client capabilities.
    Modern(Value),
}

pub struct Server {
    /// The working directory's workspace: the default root.
    base: Workspace,
    legacy: Option<Legacy>,
    previews: PreviewStore,
    requests_sent: u64,
}

impl Server {
    pub fn new(base: Workspace, preview_ttl: Duration) -> Self {
        Self {
            base,
            legacy: None,
            previews: PreviewStore::new(preview_ttl),
            requests_sent: 0,
        }
    }

    fn roots_pending(&self) -> bool {
        matches!(
            self.legacy,
            Some(Legacy {
                roots: Roots::Pending(_),
                ..
            })
        )
    }

    fn give_up_roots(&mut self) {
        if let Some(l) = &mut self.legacy {
            l.roots = Roots::NotOffered;
        }
    }

    fn handle_incoming(
        &mut self,
        incoming: Incoming,
        cancelled: &Mutex<HashSet<String>>,
    ) -> Outcome {
        let msg = match incoming {
            Incoming::Unparseable => {
                return Outcome::send(error(Value::Null, PARSE_ERROR, "Parse error", None))
            }
            Incoming::Message(m) => m,
        };
        // A cancelled request gets no response, before or after the work.
        let id_key = msg.get("id").map(Value::to_string);
        let was_cancelled = |key: &Option<String>| {
            key.as_ref()
                .is_some_and(|k| cancelled.lock().is_ok_and(|mut set| set.remove(k)))
        };
        if msg.get("method").is_some() && was_cancelled(&id_key) {
            return Outcome::default();
        }
        let mut outcome = self.handle(msg);
        if outcome.deferred.is_none() {
            outcome.send.retain(|m| {
                !(is_response(m)
                    && m.get("id").map(Value::to_string) == id_key
                    && was_cancelled(&id_key))
            });
        }
        outcome
    }

    /// Handle one parsed message.
    pub fn handle(&mut self, msg: Value) -> Outcome {
        let Some(obj) = msg.as_object() else {
            // Batches were removed in 2025-06-18; anything else isn't JSON-RPC.
            return Outcome::send(error(Value::Null, INVALID_REQUEST, "Invalid Request", None));
        };
        if obj.get("jsonrpc") != Some(&json!("2.0")) {
            let id = valid_id(obj.get("id")).cloned().unwrap_or(Value::Null);
            return Outcome::send(error(
                id,
                INVALID_REQUEST,
                "Invalid Request: jsonrpc must be \"2.0\"",
                None,
            ));
        }
        let Some(method) = obj.get("method") else {
            if is_response(&msg) {
                self.handle_response(obj);
                return Outcome::default();
            }
            let id = valid_id(obj.get("id")).cloned().unwrap_or(Value::Null);
            return Outcome::send(error(id, INVALID_REQUEST, "Invalid Request", None));
        };
        let Some(method) = method.as_str() else {
            let id = valid_id(obj.get("id")).cloned().unwrap_or(Value::Null);
            return Outcome::send(error(
                id,
                INVALID_REQUEST,
                "Invalid Request: method must be a string",
                None,
            ));
        };
        let params = match obj.get("params") {
            None => Value::Object(Map::new()),
            Some(p @ Value::Object(_)) => p.clone(),
            Some(_) => {
                let id = valid_id(obj.get("id")).cloned().unwrap_or(Value::Null);
                return Outcome::send(error(
                    id,
                    INVALID_REQUEST,
                    "Invalid Request: params must be an object",
                    None,
                ));
            }
        };
        match obj.get("id") {
            None => self.handle_notification(method),
            Some(id) => match valid_id(Some(id)) {
                Some(id) => self.handle_request(id.clone(), method, &params, &msg),
                None => Outcome::send(error(
                    Value::Null,
                    INVALID_REQUEST,
                    "Invalid Request: id must be a string or an integer",
                    None,
                )),
            },
        }
    }

    fn handle_notification(&mut self, method: &str) -> Outcome {
        match method {
            // The legacy handshake is done; ask for the roots if offered.
            "notifications/initialized" | "notifications/roots/list_changed" => {
                match &self.legacy {
                    Some(l) if l.offers_roots => self.request_roots(),
                    _ => Outcome::default(),
                }
            }
            // Cancellations are noted as they are read; everything else is
            // ignored, as JSON-RPC notifications get no response.
            _ => Outcome::default(),
        }
    }

    /// Ask a legacy client for its roots (2025-11-25 `roots/list`).
    fn request_roots(&mut self) -> Outcome {
        self.requests_sent += 1;
        let id = format!("kv-cli-roots-{}", self.requests_sent);
        if let Some(l) = &mut self.legacy {
            l.roots = Roots::Pending(id.clone());
        }
        Outcome::send(json!({ "jsonrpc": "2.0", "id": id, "method": "roots/list" }))
    }

    /// The client's answer to `roots/list`.
    fn handle_response(&mut self, obj: &Map<String, Value>) {
        let base = self.base.clone();
        let Some(l) = &mut self.legacy else { return };
        let Roots::Pending(want) = &l.roots else {
            return;
        };
        if obj.get("id").and_then(Value::as_str) != Some(want.as_str()) {
            return;
        }
        l.roots = match root_uris(obj.get("result")) {
            Some(uris) => Roots::Known(base.with_client_roots(&uris)),
            None => {
                eprintln!(
                    "kv-cli mcp: the client's roots/list answer had no roots; using the \
                     working directory as the workspace"
                );
                Roots::NotOffered
            }
        };
    }

    fn handle_request(&mut self, id: Value, method: &str, params: &Value, msg: &Value) -> Outcome {
        if method == "initialize" {
            return Outcome::send(self.initialize(id, params));
        }
        if method == "ping" {
            return Outcome::send(response(id, json!({})));
        }
        if !matches!(method, "server/discover" | "tools/list" | "tools/call") {
            return Outcome::send(error(
                id,
                METHOD_NOT_FOUND,
                &format!("Method not found: {method}"),
                None,
            ));
        }
        let era = match self.era(params) {
            Ok(e) => e,
            Err(message) => return Outcome::send(message.into_response(id)),
        };
        if method == "server/discover" && matches!(era, Era::Legacy) {
            return Outcome::send(error(
                id,
                METHOD_NOT_FOUND,
                "Method not found: server/discover",
                None,
            ));
        }
        match method {
            "server/discover" => Outcome::send(modern(
                id,
                json!({
                    "supportedVersions": [MODERN, LEGACY],
                    "capabilities": { "tools": {} },
                    "instructions": INSTRUCTIONS,
                    "ttlMs": 3_600_000,
                    "cacheScope": "public",
                }),
            )),
            "tools/list" => {
                let tools = json!({ "tools": tools::definitions() });
                Outcome::send(match era {
                    Era::Legacy => response(id, tools),
                    Era::Modern(_) => {
                        let mut r = tools;
                        r["ttlMs"] = json!(3_600_000);
                        r["cacheScope"] = json!("public");
                        modern(id, r)
                    }
                })
            }
            _ => self.call_tool(id, params, era, msg),
        }
    }

    /// The legacy handshake: answer with 2025-11-25, whatever was asked for.
    /// A client that can't speak it disconnects, as that revision specifies.
    fn initialize(&mut self, id: Value, params: &Value) -> Value {
        if !params["protocolVersion"].is_string() {
            return error(
                id,
                INVALID_PARAMS,
                "initialize needs a protocolVersion string",
                None,
            );
        }
        self.legacy = Some(Legacy {
            offers_roots: params["capabilities"].get("roots").is_some(),
            roots: Roots::NotOffered,
        });
        response(
            id,
            json!({
                "protocolVersion": LEGACY,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": server_info(),
                "instructions": INSTRUCTIONS,
            }),
        )
    }

    /// Which era serves this request: modern when it carries a protocol
    /// version in `_meta`, legacy when it doesn't and the session opened with
    /// `initialize`.
    fn era(&self, params: &Value) -> Result<Era, Refusal> {
        let meta = &params["_meta"];
        match meta.get(VERSION_KEY) {
            Some(Value::String(v)) if v == MODERN => match meta.get(CAPABILITIES_KEY) {
                Some(caps @ Value::Object(_)) => Ok(Era::Modern(caps.clone())),
                _ => Err(Refusal::Params(format!(
                    "_meta.{CAPABILITIES_KEY} is required on every {MODERN} request"
                ))),
            },
            Some(Value::String(v)) => Err(Refusal::Version(v.clone())),
            Some(_) => Err(Refusal::Params(format!(
                "_meta.{VERSION_KEY} must be a string"
            ))),
            None if self.legacy.is_some() => Ok(Era::Legacy),
            None => Err(Refusal::Params(format!(
                "_meta.{VERSION_KEY} is required: send it on every request ({MODERN}), or open \
                 with initialize ({LEGACY})"
            ))),
        }
    }

    fn call_tool(&mut self, id: Value, params: &Value, era: Era, msg: &Value) -> Outcome {
        let Some(name) = params["name"].as_str() else {
            return Outcome::send(error(
                id,
                INVALID_PARAMS,
                "tools/call needs a tool name",
                None,
            ));
        };
        let Some(uses_workspace) = tools::lookup(name) else {
            return Outcome::send(error(
                id,
                INVALID_PARAMS,
                &format!("Unknown tool: {name}"),
                None,
            ));
        };
        let args = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(a @ Value::Object(_)) => a.clone(),
            Some(_) => {
                return Outcome::send(error(
                    id,
                    INVALID_PARAMS,
                    "tools/call arguments must be an object",
                    None,
                ))
            }
        };

        // The workspace this call runs in.
        let workspace = if !uses_workspace {
            self.base.clone()
        } else {
            match &era {
                Era::Modern(caps) if caps.get("roots").is_some() => {
                    match params.get("inputResponses").and_then(|r| r.get("roots")) {
                        // The retry, with the roots asked for below.
                        Some(answer) => match root_uris(Some(answer)) {
                            Some(uris) => self.base.with_client_roots(&uris),
                            None => {
                                return Outcome::send(error(
                                    id,
                                    INVALID_PARAMS,
                                    "inputResponses.roots is not a roots/list result",
                                    None,
                                ))
                            }
                        },
                        // Ask for them: a multi round-trip request.
                        None => {
                            return Outcome::send(modern_with(
                                id,
                                "input_required",
                                json!({ "inputRequests": { "roots": { "method": "roots/list" } } }),
                            ))
                        }
                    }
                }
                Era::Modern(_) => self.base.clone(),
                Era::Legacy => match self.legacy.as_ref().map(|l| &l.roots) {
                    Some(Roots::Known(ws)) => ws.clone(),
                    Some(Roots::Pending(_)) => {
                        return Outcome {
                            send: Vec::new(),
                            deferred: Some(msg.clone()),
                        }
                    }
                    _ => self.base.clone(),
                },
            }
        };

        let result = tools::call(name, args, &workspace, &mut self.previews);
        let body = json!({
            "content": [{ "type": "text", "text": result.text }],
            "structuredContent": result.structured,
            "isError": result.is_error,
        });
        Outcome::send(match era {
            Era::Legacy => response(id, body),
            Era::Modern(_) => modern(id, body),
        })
    }
}

/// Why a request can't be served under any era.
enum Refusal {
    /// A malformed request: -32602.
    Params(String),
    /// A modern version this server doesn't support: -32022.
    Version(String),
}

impl Refusal {
    fn into_response(self, id: Value) -> Value {
        match self {
            Refusal::Params(m) => error(id, INVALID_PARAMS, &m, None),
            Refusal::Version(v) => error(
                id,
                UNSUPPORTED_VERSION,
                "Unsupported protocol version",
                Some(json!({ "supported": [MODERN, LEGACY], "requested": v })),
            ),
        }
    }
}

/// The roots in a `roots/list` result, as URIs. `None` when it isn't one.
fn root_uris(result: Option<&Value>) -> Option<Vec<String>> {
    let roots = result?.get("roots")?.as_array()?;
    Some(
        roots
            .iter()
            .filter_map(|r| r.get("uri").and_then(Value::as_str).map(str::to_string))
            .collect(),
    )
}

/// A JSON-RPC id an MCP request may carry: a string or an integer, never
/// null.
fn valid_id(id: Option<&Value>) -> Option<&Value> {
    id.filter(|v| v.is_string() || v.is_i64() || v.is_u64())
}

fn response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A complete 2026-07-28 result: `resultType`, and the server's identity in
/// `_meta`.
fn modern(id: Value, result: Value) -> Value {
    modern_with(id, "complete", result)
}

fn modern_with(id: Value, result_type: &str, mut result: Value) -> Value {
    result["resultType"] = json!(result_type);
    result["_meta"] = json!({ SERVER_INFO_KEY: server_info() });
    response(id, result)
}

fn error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut e = json!({ "code": code, "message": message });
    if let Some(d) = data {
        e["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": e })
}

#[cfg(test)]
mod tests;
