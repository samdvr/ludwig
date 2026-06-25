use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ProjectError;
use crate::game::Game;
use crate::parser;
use crate::plan;
use crate::project::Project;
use crate::prompts::{self, ExistingSpec, PeerSpec};
use crate::scaffold::{self, WriteSpecError};
use crate::verify::{self, RunOptions};

pub const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Deserialize)]
struct Request {
    #[allow(dead_code)]
    #[serde(default)]
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
pub struct ErrorObject {
    pub code: i32,
    pub message: String,
}

pub struct Server {
    explicit_project: Option<Project>,
    root_override: Option<PathBuf>,
}

impl Server {
    pub fn new(project: Option<Project>, root: Option<PathBuf>) -> Self {
        Self { explicit_project: project, root_override: root }
    }

    /// Run the server over stdin/stdout until EOF.
    pub fn run(self) -> std::io::Result<()> {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        for line in stdin.lock().lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(response) = self.handle_line(trimmed) {
                writeln!(out, "{}", serde_json::to_string(&response).unwrap_or_default())?;
                out.flush()?;
            }
        }
        Ok(())
    }

    /// Process a single request line; returns `None` for notifications (no id).
    pub fn handle_line(&self, line: &str) -> Option<ResponseValue> {
        let req: Request = match serde_json::from_str(line) {
            Ok(r) => r,
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
        let id = req.id.clone();
        let result = self.dispatch(&req.method, &req.params);
        // Notifications (no id) get no response.
        id.as_ref()?;
        Some(match result {
            Ok(value) => ResponseValue { id, payload: Ok(value) },
            Err(err) => ResponseValue { id, payload: Err(err) },
        })
    }

    fn dispatch(&self, method: &str, params: &Value) -> Result<Value, ErrorObject> {
        match method {
            "initialize" => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {}, "resources": {} },
                "serverInfo": { "name": "ludwig", "version": crate::VERSION }
            })),
            "initialized" | "notifications/initialized" => Ok(Value::Null),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_descriptors() })),
            "tools/call" => self.call_tool(params),
            "resources/list" => Ok(json!({ "resources": self.resource_descriptors() })),
            "resources/read" => self.read_resource(params),
            other => Err(ErrorObject {
                code: -32602,
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
            let path = project.find_spec_path(id).ok_or_else(|| ErrorObject {
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
        let empty = Value::Object(Default::default());
        let args = params.get("arguments").unwrap_or(&empty);

        let result: Value = match name {
            "spec.list" => self.tool_spec_list()?,
            "spec.read" => self.tool_spec_read(args)?,
            "spec.plan" => self.tool_spec_plan(args)?,
            "spec.verify" => self.tool_spec_verify(args)?,
            "spec.propose" => Value::String(self.tool_spec_propose(args)?),
            "spec.write" => self.tool_spec_write(args)?,
            "project.decompose" => Value::String(self.tool_project_decompose(args)?),
            "game.create" => self.tool_game_create(args)?,
            other => {
                return Err(ErrorObject {
                    code: -32602,
                    message: format!("unknown tool: {other}"),
                });
            }
        };
        let text = match &result {
            Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        };
        Ok(json!({ "content": [{ "type": "text", "text": text }] }))
    }

    fn tool_spec_list(&self) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let mut out: Vec<Value> = Vec::new();
        for path in project.spec_paths() {
            match parser::parse_file(&path) {
                Ok(doc) => {
                    let rel = path
                        .strip_prefix(&project.root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned();
                    out.push(json!({
                        "id": doc.id(),
                        "title": doc.frontmatter.title,
                        "status": doc.frontmatter.status.as_str(),
                        "version": doc.version(),
                        "path": rel,
                    }));
                }
                Err(e) => out.push(json!({
                    "path": path.to_string_lossy(),
                    "error": e.message,
                })),
            }
        }
        Ok(Value::Array(out))
    }

    fn tool_spec_read(&self, args: &Value) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let id = require_string(args, "id")?;
        let _ = project.find_spec_path(id).ok_or_else(|| ErrorObject {
            code: -32602,
            message: "no such spec".to_string(),
        })?;
        let brief = plan::brief_for(&project, id).map_err(project_to_rpc)?;
        Ok(serde_json::to_value(&brief.spec).unwrap_or_default())
    }

    fn tool_spec_plan(&self, args: &Value) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let id = require_string(args, "id")?;
        let brief = plan::brief_for(&project, id).map_err(project_to_rpc)?;
        Ok(serde_json::to_value(&brief).unwrap_or_default())
    }

    fn tool_spec_verify(&self, args: &Value) -> Result<Value, ErrorObject> {
        let project = self.project()?;
        let id = require_string(args, "id")?;
        let emit = args
            .get("emit_judgment_prompts")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let report = verify::Verify::new(&project)
            .run(id, RunOptions { emit_judgment_prompts: emit })
            .map_err(|e| ErrorObject {
                code: -32603,
                message: format!("verify failed: {e}"),
            })?;
        Ok(serde_json::to_value(&report).unwrap_or_default())
    }

    fn tool_spec_propose(&self, args: &Value) -> Result<String, ErrorObject> {
        let project = self.project()?;
        let slug = require_string(args, "slug")?;
        let description = require_string(args, "description")?;
        let game_name = args.get("game").and_then(|v| v.as_str());
        let peers_owned = peer_specs_for(&project, game_name);
        let peers: Vec<PeerSpec<'_>> = peers_owned
            .iter()
            .map(|(id, title)| PeerSpec { id, title })
            .collect();
        let glossary = glossary_for(&project, game_name);
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
                let rel = target
                    .strip_prefix(&project.root)
                    .unwrap_or(&target)
                    .to_string_lossy()
                    .into_owned();
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
        let existing_owned: Vec<(String, String, String)> = project
            .spec_paths()
            .iter()
            .filter_map(|p| {
                parser::parse_file(p).ok().map(|d| {
                    (
                        d.id().to_string(),
                        d.frontmatter.title.clone(),
                        d.frontmatter.status.as_str().to_string(),
                    )
                })
            })
            .collect();
        let existing: Vec<ExistingSpec<'_>> = existing_owned
            .iter()
            .map(|(id, title, status)| ExistingSpec {
                id,
                title,
                status,
            })
            .collect();
        let mut games: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(project.specs_dir()) {
            for e in rd.flatten() {
                if e.path().is_dir()
                    && let Some(n) = e.file_name().to_str()
                {
                    games.push(n.to_string());
                }
            }
        }
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
                let rel = target
                    .strip_prefix(&project.root)
                    .unwrap_or(&target)
                    .to_string_lossy()
                    .into_owned();
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
        let (result, error) = match &self.payload {
            Ok(v) => (Some(v.clone()), None),
            Err(e) => (
                None,
                Some(json!({ "code": e.code, "message": e.message })),
            ),
        };
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.id,
            "result": result,
            "error": error,
        });
        // Drop nulls so we don't ship both result and error keys.
        let cleaned = drop_null_keys(resp);
        cleaned.serialize(s)
    }
}

fn drop_null_keys(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                if val.is_null() && (k == "result" || k == "error") {
                    continue;
                }
                out.insert(k, val);
            }
            Value::Object(out)
        }
        other => other,
    }
}

fn project_to_rpc(e: ProjectError) -> ErrorObject {
    ErrorObject { code: -32603, message: e.0 }
}

fn require_string<'a>(args: &'a Value, key: &str) -> Result<&'a str, ErrorObject> {
    args.get(key).and_then(|v| v.as_str()).ok_or_else(|| ErrorObject {
        code: -32602,
        message: format!("missing argument: {key}"),
    })
}

fn peer_specs_for(project: &Project, game_name: Option<&str>) -> Vec<(String, String)> {
    let dir = match game_name {
        Some(g) => project.specs_dir().join(g),
        None => project.specs_dir(),
    };
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut out: Vec<(String, String)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if !p
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(".spec.md"))
                .unwrap_or(false)
            {
                continue;
            }
            if let Ok(doc) = parser::parse_file(&p) {
                out.push((doc.id().to_string(), doc.frontmatter.title.clone()));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn glossary_for(project: &Project, game_name: Option<&str>) -> Vec<(String, String)> {
    let Some(g) = game_name else { return Vec::new() };
    let manifest = project.specs_dir().join(g).join(Game::MANIFEST_FILE);
    if !manifest.is_file() {
        return Vec::new();
    }
    match Game::load(&manifest, project) {
        Ok(game) => game.glossary.into_iter().collect(),
        Err(_) => Vec::new(),
    }
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
