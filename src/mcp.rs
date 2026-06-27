use std::io::{BufRead, Read, Write};
use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Value, json};

use crate::drift;
use crate::error::ProjectError;
use crate::parser;
use crate::plan;
use crate::project::Project;
use crate::prompts::{self, ExistingSpec, PeerSpec};
use crate::scaffold::{self, WriteSpecError};
use crate::verify::{self, IngestedVerdict, RunOptions};

pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Protocol versions this server can speak, newest first. On `initialize` we
/// echo the client's requested version if it is one of these, otherwise we fall
/// back to [`PROTOCOL_VERSION`] so the client knows which version we'll use.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[PROTOCOL_VERSION];

/// Pick the protocol version to report from `initialize`. Echoes the client's
/// requested version when we support it; otherwise returns our default.
fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    match requested {
        Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v) => {
            SUPPORTED_PROTOCOL_VERSIONS.iter().find(|s| **s == v).unwrap()
        }
        _ => PROTOCOL_VERSION,
    }
}

/// Canonical list of MCP tool names this server exposes. Both
/// [`tool_descriptors`] (advertised over `tools/list`) and [`Server::call_tool`]
/// (the dispatcher) must agree with this list — the `tools_descriptors_match_dispatch`
/// test enforces it. Adding a new tool means: append a name here, add a
/// descriptor in `tool_descriptors`, and add a match arm in `call_tool`.
pub const TOOL_NAMES: &[&str] = &[
    "spec.list",
    "spec.read",
    "spec.plan",
    "spec.verify",
    "spec.diff",
    "spec.propose",
    "spec.write",
    "spec.move",
    "spec.ingest_judgments",
    "project.decompose",
    "game.create",
];

/// Tools that compile and execute project code as a side effect. `spec.verify`
/// shells out to `cargo test`, which runs whatever lives in `tests/ludwig_*.rs`
/// — arbitrary code execution. When the server is locked down (`--no-exec`)
/// these are hidden from `tools/list` and refused by the dispatcher, leaving the
/// read-only spec tools available to an untrusted client.
const EXEC_TOOLS: &[&str] = &["spec.verify"];

/// Per-line read cap for the stdio transport. A JSON-RPC message arrives as a
/// single line; bounding it stops a newline-less line from an untrusted client
/// from exhausting memory. 16 MiB sits far above any legitimate request (e.g. a
/// large `spec.write` body) while staying finite.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Consume and discard bytes up to and including the next newline (or EOF)
/// without buffering them. Used to resynchronize the stream after an over-long
/// line is rejected, so the next [`Server::run`] iteration starts at a fresh
/// message boundary. Allocation-free: it works directly off the reader's buffer.
fn drain_line<R: BufRead>(reader: &mut R) -> std::io::Result<()> {
    loop {
        let (done, used) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Ok(()); // EOF
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(i) => (true, i + 1),
                None => (false, available.len()),
            }
        };
        reader.consume(used);
        if done {
            return Ok(());
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
}

pub struct Server {
    explicit_project: Option<Project>,
    root_override: Option<PathBuf>,
    /// When false, code-executing tools ([`EXEC_TOOLS`]) are unavailable. Default
    /// true to preserve the local-developer experience; set via [`Server::with_exec`].
    allow_exec: bool,
}

impl Server {
    pub fn new(project: Option<Project>, root: Option<PathBuf>) -> Self {
        Self { explicit_project: project, root_override: root, allow_exec: true }
    }

    /// Enable or disable code-executing tools. Pass `false` when exposing the
    /// server to an untrusted client to drop `spec.verify` from the surface.
    pub fn with_exec(mut self, allow: bool) -> Self {
        self.allow_exec = allow;
        self
    }

    /// Run the server over stdin/stdout until EOF.
    pub fn run(self) -> std::io::Result<()> {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let mut reader = stdin.lock();
        let mut buf: Vec<u8> = Vec::new();
        loop {
            buf.clear();
            // Bound each line with `take` so a single newline-less line from an
            // untrusted client can't drive unbounded allocation (memory-exhaustion
            // DoS). A JSON-RPC message arrives as one line, so the cap is per
            // message. `.lines()` would grow the buffer without limit.
            let n = (&mut reader)
                .take(MAX_LINE_BYTES as u64)
                .read_until(b'\n', &mut buf)?;
            if n == 0 {
                break; // EOF
            }
            // Hit the cap without reaching a newline: the line is over-long.
            // Reject it, drain its tail so the stream resynchronizes at the next
            // message boundary, and keep serving.
            if n == MAX_LINE_BYTES && buf.last() != Some(&b'\n') {
                drain_line(&mut reader)?;
                let resp = serialize_response(&ResponseValue {
                    id: None,
                    payload: Err(ErrorObject {
                        code: -32600,
                        message: format!(
                            "invalid request: line exceeds {MAX_LINE_BYTES}-byte limit"
                        ),
                    }),
                });
                writeln!(out, "{resp}")?;
                out.flush()?;
                continue;
            }
            let line = String::from_utf8_lossy(&buf);
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(payload) = self.handle_payload(trimmed) {
                writeln!(out, "{payload}")?;
                out.flush()?;
            }
        }
        Ok(())
    }

    /// Handle one input line — a single JSON-RPC request object or a batch
    /// (array of them) — and return the serialized response line to write, or
    /// `None` when nothing should be written (a lone notification, or a batch
    /// composed entirely of notifications). Batches respond with a JSON array of
    /// responses, excluding notifications, per JSON-RPC 2.0.
    fn handle_payload(&self, line: &str) -> Option<String> {
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(serialize_response(&ResponseValue {
                    id: None,
                    payload: Err(ErrorObject {
                        code: -32700,
                        message: format!("parse error: {e}"),
                    }),
                }));
            }
        };
        match value {
            Value::Array(items) => {
                if items.is_empty() {
                    // An empty batch array is itself an invalid request.
                    return Some(serialize_response(&ResponseValue {
                        id: None,
                        payload: Err(ErrorObject {
                            code: -32600,
                            message: "invalid request: empty batch".to_string(),
                        }),
                    }));
                }
                let responses: Vec<ResponseValue> =
                    items.into_iter().filter_map(|item| self.handle_value(item)).collect();
                if responses.is_empty() {
                    None
                } else {
                    Some(serialize_batch(&responses))
                }
            }
            other => self.handle_value(other).map(|r| serialize_response(&r)),
        }
    }

    /// Process a single request line; returns `None` for notifications (a
    /// message with no `id` member). An explicit `"id": null` is a request and
    /// gets a response, per JSON-RPC 2.0.
    pub fn handle_line(&self, line: &str) -> Option<ResponseValue> {
        let value: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(ResponseValue {
                    id: None,
                    payload: Err(ErrorObject {
                        code: -32700,
                        message: format!("parse error: {e}"),
                    }),
                });
            }
        };
        self.handle_value(value)
    }

    /// Dispatch one already-parsed JSON-RPC message value. Returns `None` for
    /// notifications (no `id` member); otherwise a response to serialize.
    fn handle_value(&self, value: Value) -> Option<ResponseValue> {
        let obj = match value.as_object() {
            Some(o) => o,
            None => {
                return Some(ResponseValue {
                    id: None,
                    payload: Err(ErrorObject {
                        code: -32600,
                        message: "invalid request: expected a JSON object".to_string(),
                    }),
                });
            }
        };
        // Notifications are distinguished by the *absence* of `id`; a present
        // `id` (even `null`) means a response is expected.
        let has_id = obj.contains_key("id");
        let id = obj.get("id").cloned();
        let method = match obj.get("method").and_then(|m| m.as_str()) {
            Some(m) => m.to_string(),
            None => {
                // Structurally valid JSON but not a valid Request → -32600.
                // The id (null when absent) is echoed back per the spec.
                return Some(ResponseValue {
                    id,
                    payload: Err(ErrorObject {
                        code: -32600,
                        message: "invalid request: missing method".to_string(),
                    }),
                });
            }
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let result = self.dispatch(&method, &params);
        // Notifications (no id member) get no response.
        if !has_id {
            return None;
        }
        Some(match result {
            Ok(value) => ResponseValue { id, payload: Ok(value) },
            Err(err) => ResponseValue { id, payload: Err(err) },
        })
    }

    fn dispatch(&self, method: &str, params: &Value) -> Result<Value, ErrorObject> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": negotiate_protocol_version(
                    params.get("protocolVersion").and_then(|v| v.as_str())
                ),
                "capabilities": { "tools": {}, "resources": {} },
                "serverInfo": { "name": "ludwig", "version": crate::VERSION }
            })),
            "initialized" | "notifications/initialized" => Ok(Value::Null),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.advertised_tools() })),
            "tools/call" => self.call_tool(params),
            "resources/list" => Ok(json!({ "resources": self.resource_descriptors() })),
            "resources/read" => self.read_resource(params),
            other => Err(ErrorObject {
                code: -32601,
                message: format!("method not found: {other}"),
            }),
        }
    }

    fn project(&self) -> Result<Project, ErrorObject> {
        if let Some(p) = &self.explicit_project {
            return Ok(p.clone());
        }
        let start = self
            .root_override
            .clone()
            .or_else(|| std::env::var_os("LUDWIG_PROJECT").map(PathBuf::from))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        Project::discover(&start).map_err(|e| ErrorObject {
            code: -32001,
            message: format!(
                "no Ludwig project: {}. Set LUDWIG_PROJECT=/abs/path or `cd` into a project.",
                e.0
            ),
        })
    }

    fn project_available(&self) -> Option<Project> {
        self.project().ok()
    }

    /// The tool descriptors to advertise over `tools/list`, dropping
    /// code-executing tools when the server is locked down so a client never
    /// sees a tool it would be refused.
    fn advertised_tools(&self) -> Vec<Value> {
        tool_descriptors()
            .into_iter()
            .filter(|d| {
                let name = d.get("name").and_then(|n| n.as_str()).unwrap_or("");
                self.allow_exec || !EXEC_TOOLS.contains(&name)
            })
            .collect()
    }

    fn resource_descriptors(&self) -> Vec<Value> {
        let project = match self.project_available() {
            Some(p) => p,
            None => return Vec::new(),
        };
        let mut out: Vec<Value> = Vec::new();
        for path in project.spec_paths() {
            if let Ok(doc) = parser::parse_file(&path) {
                out.push(json!({
                    "uri": format!("ludwig://spec/{}", doc.id()),
                    "name": doc.frontmatter.title,
                    "description": format!(
                        "Ludwig spec {} (v{}, {})",
                        doc.id(),
                        doc.version(),
                        doc.frontmatter.status.as_str()
                    ),
                    "mimeType": "text/markdown"
                }));
            }
        }
        let latest = project.reports_dir().join("latest.md");
        if latest.is_file() {
            out.push(json!({
                "uri": "ludwig://report/latest",
                "name": "Latest verification report",
                "description": "Most recent ludwig verify output",
                "mimeType": "text/markdown"
            }));
        }
        out
    }

    fn read_resource(&self, params: &Value) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let uri = params
            .get("uri")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorObject {
                code: -32602,
                message: "missing uri".to_string(),
            })?;
        if let Some(id) = uri.strip_prefix("ludwig://spec/") {
            let path = project.find_spec_by_id(id).ok_or_else(|| ErrorObject {
                code: -32602,
                message: "no such spec".to_string(),
            })?;
            let text = std::fs::read_to_string(&path).map_err(|e| ErrorObject {
                code: -32603,
                message: format!("read failed: {e}"),
            })?;
            return Ok(json!({
                "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }]
            }));
        }
        if uri == "ludwig://report/latest" {
            let latest = project.reports_dir().join("latest.md");
            let text = std::fs::read_to_string(&latest).map_err(|_| ErrorObject {
                code: -32602,
                message: "no report yet; run `ludwig verify`".to_string(),
            })?;
            return Ok(json!({
                "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }]
            }));
        }
        Err(ErrorObject {
            code: -32602,
            message: format!("unknown uri: {uri}"),
        })
    }

    fn call_tool(&self, params: &Value) -> Result<Value, ErrorObject> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ErrorObject {
                code: -32602,
                message: "missing tool name".to_string(),
            })?;
        // Refuse code-executing tools when locked down. Reported as a -32602
        // (invalid params) so the calling model sees it as "this tool isn't
        // available here", consistent with how `tools/list` hides it.
        if !self.allow_exec && EXEC_TOOLS.contains(&name) {
            return Err(ErrorObject {
                code: -32602,
                message: format!(
                    "tool {name:?} is disabled: this server was started with --no-exec"
                ),
            });
        }
        let empty = Value::Object(Default::default());
        let args = params.get("arguments").unwrap_or(&empty);

        let result: Result<Value, ErrorObject> = match name {
            "spec.list" => self.tool_spec_list(),
            "spec.read" => self.tool_spec_read(args),
            "spec.plan" => self.tool_spec_plan(args),
            "spec.verify" => self.tool_spec_verify(args),
            "spec.diff" => self.tool_spec_diff(args),
            "spec.propose" => self.tool_spec_propose(args).map(Value::String),
            "spec.write" => self.tool_spec_write(args),
            "spec.move" => self.tool_spec_move(args),
            "spec.ingest_judgments" => self.tool_spec_ingest_judgments(args),
            "project.decompose" => self.tool_project_decompose(args).map(Value::String),
            "game.create" => self.tool_game_create(args),
            other => {
                return Err(ErrorObject {
                    code: -32602,
                    message: format!("unknown tool: {other}"),
                });
            }
        };
        match result {
            Ok(value) => {
                let text = match &value {
                    Value::String(s) => s.clone(),
                    other => serde_json::to_string_pretty(other).unwrap_or_default(),
                };
                Ok(json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false
                }))
            }
            // Malformed-params failures (-32602) stay protocol-level JSON-RPC
            // errors. Genuine tool-execution failures (verify failed, no such
            // spec, internal errors) surface as a tool result with
            // `isError: true` so the calling model can see and react to them,
            // per the MCP spec — they are not transport failures.
            Err(err) if err.code == -32602 => Err(err),
            Err(err) => Ok(json!({
                "content": [{ "type": "text", "text": err.message }],
                "isError": true,
                // Preserve the originating JSON-RPC error code so a client that
                // branches on it (e.g. -32001 "no project") still can — it would
                // otherwise be lost when the error is folded into a text result.
                "code": err.code
            })),
        }
    }

    fn tool_spec_list(&self) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let mut out: Vec<Value> = Vec::new();
        for path in project.spec_paths() {
            match parser::parse_file(&path) {
                Ok(doc) => {
                    let rel = crate::util::rel_str(&project.root, &path);
                    out.push(json!({
                        "id": doc.id(),
                        "title": doc.frontmatter.title,
                        "status": doc.frontmatter.status.as_str(),
                        "version": doc.version(),
                        "path": rel,
                    }));
                }
                Err(e) => {
                    let rel = crate::util::rel_str(&project.root, &path);
                    out.push(json!({
                        "path": rel,
                        "error": e.message,
                    }))
                }
            }
        }
        Ok(Value::Array(out))
    }

    fn tool_spec_read(&self, args: &Value) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let id = require_string(args, "id")?;
        let path = project.find_spec_by_id(id).ok_or_else(|| ErrorObject {
            code: -32602,
            message: "no such spec".to_string(),
        })?;
        let doc = parser::parse_file(&path).map_err(|e| ErrorObject {
            code: -32603,
            message: e.message,
        })?;
        let rel = crate::util::rel_str(&project.root, &path);
        // Mirror the shape of `plan::SpecBrief` so MCP callers don't have to
        // special-case the two tools. Avoids the cost of full dependency
        // resolution and file fingerprints that `plan::brief_for` performs.
        Ok(json!({
            "id": doc.id(),
            "title": doc.frontmatter.title,
            "version": doc.version(),
            "status": doc.frontmatter.status.as_str(),
            "canonical_hash": doc.canonical_hash(),
            "path": rel,
            "intent": doc.intent,
            "behaviors": doc.behaviors,
            "examples": doc.examples,
            "invariants": doc.invariants,
            "non_goals": doc.non_goals,
            "implementation_notes": doc.implementation_notes,
        }))
    }

    fn tool_spec_plan(&self, args: &Value) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let id = require_string(args, "id")?;
        let path = confine_spec_id(&project, id)?;
        let brief = plan::brief_for_path(&project, &path).map_err(project_to_rpc)?;
        Ok(serde_json::to_value(&brief).unwrap_or_default())
    }

    fn tool_spec_verify(&self, args: &Value) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let id = require_string(args, "id")?;
        let path = confine_spec_id(&project, id)?;
        let emit = args
            .get("emit_judgment_prompts")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let report = verify::Verify::new(&project)
            .run_path(&path, RunOptions { emit_judgment_prompts: emit })
            .map_err(|e| ErrorObject {
                code: -32603,
                message: format!("verify failed: {e}"),
            })?;
        Ok(serde_json::to_value(&report).unwrap_or_default())
    }

    fn tool_spec_diff(&self, args: &Value) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let id = require_string(args, "id")?;
        let path = confine_spec_id(&project, id)?;
        let report = drift::report_for_path(&project, &path).map_err(project_to_rpc)?;
        Ok(serde_json::to_value(&report).unwrap_or_default())
    }

    fn tool_spec_ingest_judgments(&self, args: &Value) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let verdicts_value = args.get("verdicts").ok_or_else(|| ErrorObject {
            code: -32602,
            message: "missing argument: verdicts (array)".to_string(),
        })?;
        // Re-deserialize through the IngestedVerdict type so the same shape is
        // accepted as the file path overload — keeps a single source of truth
        // for what a "verdict" looks like.
        let verdicts: Vec<IngestedVerdict> = serde_json::from_value(verdicts_value.clone())
            .map_err(|e| ErrorObject {
                code: -32602,
                message: format!("verdicts must be an array of {{invariant_key, verdict, ...}}: {e}"),
            })?;
        let count = verdicts.len();
        // Flag verdicts whose key matches no judgment invariant currently in the
        // project. We still persist them (a later spec edit might reintroduce the
        // key, and the file overload preserves everything), but the agent gets
        // told so a typo'd key isn't silently stuck pending at verify time.
        let known = known_judgment_keys(&project);
        let unknown: Vec<String> = verdicts
            .iter()
            .map(|v| v.invariant_key.clone())
            .filter(|k| !known.contains(k))
            .collect();
        verify::Verify::new(&project)
            .apply_judgments(verdicts)
            .map_err(|e| ErrorObject {
                code: -32603,
                message: format!("ingest failed: {e}"),
            })?;
        Ok(json!({ "ok": true, "ingested": count, "unknown": unknown }))
    }

    fn tool_spec_move(&self, args: &Value) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let slug = require_string(args, "slug")?;
        // `to_game` is intentionally optional — passing `null` (or omitting)
        // means "move to the specs root".
        let to_game = args.get("to_game").and_then(|v| v.as_str());
        let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
        match scaffold::move_spec(&project, slug, to_game, force) {
            Ok(target) => {
                let rel = crate::util::rel_str(&project.root, &target);
                Ok(json!({ "ok": true, "path": rel }))
            }
            Err(e) => Ok(json!({ "ok": false, "error": e.0 })),
        }
    }

    fn tool_spec_propose(&self, args: &Value) -> Result<String, ErrorObject> {
        let project = self.project()?;
        let slug = require_string(args, "slug")?;
        let description = require_string(args, "description")?;
        let game_name = args.get("game").and_then(|v| v.as_str());
        // `game` flows into `specs_dir().join(game)` to enumerate peer specs and
        // read the game glossary, so it must be confined exactly as spec.write /
        // spec.move / game.create confine it — an unvalidated value like
        // "../../somedir" would escape the specs directory and leak the existence
        // and glossary of files outside it.
        if let Some(g) = game_name {
            scaffold::validate_slug(g).map_err(|e| ErrorObject {
                code: -32602,
                message: e.0,
            })?;
        }
        let peers_owned = project.peer_specs_for(game_name);
        let peers: Vec<PeerSpec<'_>> = peers_owned
            .iter()
            .map(|(id, title)| PeerSpec { id, title })
            .collect();
        let glossary = project.glossary_for(game_name);
        Ok(prompts::spec_from_description(
            slug,
            description,
            game_name,
            &peers,
            &glossary,
        ))
    }

    fn tool_spec_write(&self, args: &Value) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let slug = require_string(args, "slug")?;
        let content = require_string(args, "content")?;
        let game = args.get("game").and_then(|v| v.as_str());
        let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
        match scaffold::write_spec(&project, slug, content, game, force) {
            Ok(target) => {
                let rel = crate::util::rel_str(&project.root, &target);
                Ok(json!({
                    "ok": true,
                    "path": rel,
                    "id": slug,
                    "next_step": "Review the spec; flip `status: draft` → `status: active`; then call `spec.plan` to produce the generation brief."
                }))
            }
            Err(WriteSpecError::Parse(e)) => Ok(json!({
                "ok": false,
                "error": e.message,
                "hint": "fix the markdown and call spec.write again"
            })),
            Err(WriteSpecError::Project(e)) => Ok(json!({
                "ok": false,
                "error": e.0,
                "hint": "fix the markdown and call spec.write again"
            })),
        }
    }

    fn tool_project_decompose(&self, args: &Value) -> Result<String, ErrorObject> {
        let project = self.project()?;
        let description = require_string(args, "description")?;
        let existing_owned = project.list_existing_specs();
        let existing: Vec<ExistingSpec<'_>> = existing_owned
            .iter()
            .map(|(id, title, status)| ExistingSpec {
                id,
                title,
                status,
            })
            .collect();
        let games = project.list_existing_games();
        Ok(prompts::project_decomposition(description, &existing, &games))
    }

    fn tool_game_create(&self, args: &Value) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let name = require_string(args, "name")?;
        let intent = args.get("intent").and_then(|v| v.as_str());
        let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
        let glossary_pairs: Vec<(String, String)> = match args.get("glossary") {
            Some(Value::Object(m)) => m
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect(),
            _ => Vec::new(),
        };
        match scaffold::create_game(&project, name, intent, &glossary_pairs, force) {
            Ok(target) => {
                let rel = crate::util::rel_str(&project.root, &target);
                Ok(json!({ "ok": true, "path": rel }))
            }
            Err(e) => Ok(json!({ "ok": false, "error": e.0 })),
        }
    }
}

#[derive(Debug)]
pub struct ResponseValue {
    pub id: Option<Value>,
    pub payload: Result<Value, ErrorObject>,
}

impl serde::Serialize for ResponseValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // A JSON-RPC response MUST carry exactly one of `result` or `error`, and
        // always an `id` (null when the request id was unknown). Build the map
        // with only the relevant member so a legitimately-null result (e.g. the
        // response to a request-shaped `initialized`) is never dropped, which
        // would leave a response with neither member.
        let mut map = serde_json::Map::new();
        map.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
        map.insert(
            "id".to_string(),
            self.id.clone().unwrap_or(Value::Null),
        );
        match &self.payload {
            Ok(v) => {
                map.insert("result".to_string(), v.clone());
            }
            Err(e) => {
                map.insert(
                    "error".to_string(),
                    json!({ "code": e.code, "message": e.message }),
                );
            }
        }
        Value::Object(map).serialize(s)
    }
}

fn project_to_rpc(e: ProjectError) -> ErrorObject {
    ErrorObject { code: -32603, message: e.0 }
}

/// Collect every judgment-invariant key the project currently defines, in the
/// `<spec-id>::judgment::<n>` form that `verify` emits. Used to flag ingested
/// verdicts whose key matches no known invariant.
fn known_judgment_keys(project: &Project) -> std::collections::HashSet<String> {
    let mut keys = std::collections::HashSet::new();
    for path in project.spec_paths() {
        if let Ok(doc) = parser::parse_file(&path) {
            let n = doc.judgment_invariants().count();
            for i in 1..=n {
                keys.insert(format!("{}::judgment::{}", doc.id(), i));
            }
        }
    }
    keys
}

/// Confine an MCP-supplied spec id to the project and return its resolved path.
/// Ids arrive straight from the client, and `plan`/`verify`/`drift` all accept
/// an id-*or-path*, so before we hand work to those resolvers we confirm the id
/// names a real spec inside the specs directory via [`Project::find_spec_by_id`]
/// and pass the resolved path straight through — both confining the lookup and
/// sparing a second full scan of the specs tree. A traversal or absolute-path id
/// matches nothing and is rejected with a plain "no such spec" — we deliberately
/// do not echo the probed filesystem path back to the client.
fn confine_spec_id(project: &Project, id: &str) -> Result<PathBuf, ErrorObject> {
    project.find_spec_by_id(id).ok_or_else(|| ErrorObject {
        code: -32602,
        message: "no such spec".to_string(),
    })
}

/// Serialize a response to a JSON-RPC line. The response types are constructed
/// from `serde_json::Value` internally, so serialization should be infallible —
/// but if it ever isn't, we MUST emit a well-formed error line rather than an
/// empty string. An empty line on stdout would corrupt the JSON-RPC framing the
/// client uses to delimit messages.
fn serialize_response(response: &ResponseValue) -> String {
    match serde_json::to_string(response) {
        Ok(s) => s,
        Err(e) => {
            let fallback = serde_json::json!({
                "jsonrpc": "2.0",
                "id": response.id.clone(),
                "error": {
                    "code": -32603,
                    "message": format!("internal serialization error: {e}"),
                },
            });
            // serde_json::to_string on a plain Value is itself effectively
            // infallible; fall back to a hand-built byte string only if it
            // somehow isn't.
            serde_json::to_string(&fallback).unwrap_or_else(|_| {
                r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"internal serialization error"}}"#
                    .to_string()
            })
        }
    }
}

/// Serialize a batch of responses to a single JSON-RPC array line. Mirrors
/// [`serialize_response`]'s guarantee of always emitting well-formed JSON so the
/// stdout framing the client relies on is never corrupted.
fn serialize_batch(responses: &[ResponseValue]) -> String {
    match serde_json::to_string(responses) {
        Ok(s) => s,
        Err(_) => {
            let parts: Vec<String> = responses.iter().map(serialize_response).collect();
            format!("[{}]", parts.join(","))
        }
    }
}

fn require_string<'a>(args: &'a Value, key: &str) -> Result<&'a str, ErrorObject> {
    args.get(key).and_then(|v| v.as_str()).ok_or_else(|| ErrorObject {
        code: -32602,
        message: format!("missing argument: {key}"),
    })
}

fn tool_descriptors() -> Vec<Value> {
    vec![
        json!({
            "name": "spec.list",
            "description": "List all Ludwig specs in this project.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "spec.read",
            "description": "Return the parsed structure of a spec by id.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "spec.plan",
            "description": "Return the generation brief for a spec by id.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "spec.verify",
            "description": "Run the verification pipeline for a spec by id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "emit_judgment_prompts": { "type": "boolean" }
                },
                "required": ["id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "spec.diff",
            "description": "Return drift between a spec and its implementing files (stale stamps, missing files, body changes).",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "spec.propose",
            "description": "Return a prompt for drafting a new spec from a description.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "slug": { "type": "string" },
                    "description": { "type": "string" },
                    "game": { "type": ["string", "null"] }
                },
                "required": ["slug", "description"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "spec.write",
            "description": "Validate a complete spec markdown document and persist it under specs/.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "slug": { "type": "string" },
                    "content": { "type": "string" },
                    "game": { "type": ["string", "null"] },
                    "force": { "type": "boolean" }
                },
                "required": ["slug", "content"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "spec.move",
            "description": "Move an existing spec into a different game (or to the specs root if to_game is null).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "slug": { "type": "string" },
                    "to_game": { "type": ["string", "null"] },
                    "force": { "type": "boolean" }
                },
                "required": ["slug"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "spec.ingest_judgments",
            "description": "Persist a batch of judgment verdicts inline (no file path). Use after evaluating prompts emitted by spec.verify with emit_judgment_prompts=true.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "verdicts": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "invariant_key": { "type": "string" },
                                "verdict": { "type": "string", "enum": ["pass", "fail"] },
                                "rationale": { "type": ["string", "null"] },
                                "spec_id": { "type": ["string", "null"] },
                                "spec_hash": { "type": ["string", "null"] }
                            },
                            "required": ["invariant_key", "verdict"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["verdicts"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "project.decompose",
            "description": "Return a prompt that decomposes a project description into specs.",
            "inputSchema": {
                "type": "object",
                "properties": { "description": { "type": "string" } },
                "required": ["description"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "game.create",
            "description": "Create a language-game (specs/<name>/_game.md) with an optional glossary.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "intent": { "type": "string" },
                    "glossary": { "type": "object", "additionalProperties": { "type": "string" } },
                    "force": { "type": "boolean" }
                },
                "required": ["name"],
                "additionalProperties": false
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Guard against the descriptor table and the dispatch match getting out of
    /// sync. Both must cover exactly `TOOL_NAMES`. If you add a tool, update all
    /// three places — this test will tell you if you missed one.
    #[test]
    fn tools_descriptors_match_dispatch() {
        let canonical: BTreeSet<&str> = TOOL_NAMES.iter().copied().collect();

        let descriptors = tool_descriptors();
        let advertised: BTreeSet<String> = descriptors
            .iter()
            .filter_map(|d| d.get("name").and_then(|n| n.as_str()).map(String::from))
            .collect();
        let advertised_refs: BTreeSet<&str> = advertised.iter().map(String::as_str).collect();
        assert_eq!(
            advertised_refs, canonical,
            "tool_descriptors() must advertise exactly TOOL_NAMES",
        );

        // Verify the dispatcher knows every canonical name. We build a fake
        // request and confirm that `call_tool` does NOT return "unknown tool".
        // A descriptor that's missing from the match arm would fail here.
        for name in TOOL_NAMES {
            // We don't have a real project, so this will most likely fail with
            // some other error — but never with "unknown tool".
            let probe = json!({ "name": name, "arguments": {} });
            let server = Server::new(None, None);
            let err = server.call_tool(&probe).err();
            if let Some(e) = err {
                assert!(
                    !e.message.starts_with("unknown tool:"),
                    "call_tool() rejected canonical name {name:?} as unknown — \
                     add a match arm for it",
                );
            }
        }
    }

    #[test]
    fn batch_request_returns_array_of_responses() {
        let server = Server::new(None, None);
        let batch = r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}]"#;
        let payload = server.handle_payload(batch).expect("batch yields a response");
        let value: Value = serde_json::from_str(&payload).unwrap();
        let arr = value.as_array().expect("batch response is an array");
        assert_eq!(arr.len(), 2);
        // Every element carries exactly one of result/error, and an id.
        for resp in arr {
            assert!(resp.get("id").is_some());
            assert_ne!(
                resp.get("result").is_some(),
                resp.get("error").is_some(),
                "exactly one of result/error: {resp}"
            );
        }
    }

    #[test]
    fn batch_of_only_notifications_yields_no_response() {
        let server = Server::new(None, None);
        // No `id` member → notifications → no responses, so nothing is written.
        let batch = r#"[{"jsonrpc":"2.0","method":"initialized"},{"jsonrpc":"2.0","method":"ping"}]"#;
        assert!(server.handle_payload(batch).is_none());
    }

    #[test]
    fn empty_batch_is_invalid_request() {
        let server = Server::new(None, None);
        let payload = server.handle_payload("[]").expect("empty batch gets an error");
        let value: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value.pointer("/error/code"), Some(&Value::from(-32600)));
    }

    #[test]
    fn request_shaped_notification_keeps_a_result_member() {
        // `initialized` dispatched WITH an id returns Ok(Value::Null). The
        // response must still carry a `result` member (not be dropped), so it
        // stays a well-formed JSON-RPC response.
        let server = Server::new(None, None);
        let resp = server
            .handle_line(r#"{"jsonrpc":"2.0","id":9,"method":"initialized"}"#)
            .expect("a response");
        let value = serde_json::to_value(&resp).unwrap();
        assert!(value.get("result").is_some(), "result member present: {value}");
        assert!(value.get("error").is_none());
        assert_eq!(value.get("id"), Some(&Value::from(9)));
    }
}
