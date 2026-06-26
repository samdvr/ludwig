mod common;

use common::TempDir;
use serde_json::{Value, json};

fn make_project_with_minimal_spec() -> (TempDir, ludwig::project::Project) {
    let dir = TempDir::new("ludwig-test");
    ludwig::scaffold::init(dir.path()).unwrap();
    let project = ludwig::project::Project::open(dir.path()).unwrap();
    let fixture = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/specs/valid/minimal.spec.md"
    ))
    .unwrap();
    std::fs::write(project.specs_dir().join("minimal.spec.md"), fixture).unwrap();
    (dir, project)
}

fn call(server: &ludwig::mcp::Server, method: &str, params: Value) -> Value {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let line = serde_json::to_string(&request).unwrap();
    let resp = server.handle_line(&line).expect("response");
    serde_json::to_value(&resp).unwrap()
}

#[test]
fn initialize_returns_capabilities() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(&server, "initialize", json!({}));
    assert_eq!(
        resp.pointer("/result/serverInfo/name"),
        Some(&Value::String("ludwig".to_string()))
    );
    assert!(resp.pointer("/result/capabilities/tools").is_some());
}

#[test]
fn tools_list_includes_core_tools() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(&server, "tools/list", json!({}));
    let names: Vec<String> = resp
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|t| t.get("name")?.as_str().map(String::from)).collect())
        .unwrap_or_default();
    for expected in &[
        "spec.list",
        "spec.read",
        "spec.plan",
        "spec.verify",
        "spec.propose",
        "spec.write",
        "project.decompose",
        "game.create",
    ] {
        assert!(names.iter().any(|n| n == expected), "missing tool: {expected}");
    }
}

#[test]
fn spec_list_returns_known_spec() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(
        &server,
        "tools/call",
        json!({ "name": "spec.list", "arguments": {} }),
    );
    let text = resp.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    let ids: Vec<String> = parsed
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.get("id")?.as_str().map(String::from))
        .collect();
    assert!(ids.iter().any(|s| s == "hello-greeter"));
}

#[test]
fn resources_list_includes_spec_uris() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(&server, "resources/list", json!({}));
    let uris: Vec<String> = resp
        .pointer("/result/resources")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r.get("uri")?.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert!(uris.iter().any(|u| u == "ludwig://spec/hello-greeter"));
}

#[test]
fn resources_read_returns_markdown() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(
        &server,
        "resources/read",
        json!({ "uri": "ludwig://spec/hello-greeter" }),
    );
    let text = resp.pointer("/result/contents/0/text").and_then(Value::as_str).unwrap();
    assert!(text.contains("## Intent"));
}

// -- spec: mcp-path-confinement ----------------------------------------------
// The MCP server resolves ids/URIs straight from the client, so a lookup must
// never read a file outside the project's specs directory.

/// {#b2}/{#b3} + example "parent traversal is rejected": `..` segments in a
/// resource URI must not escape the project; no out-of-root file is returned.
#[test]
fn resources_read_rejects_parent_traversal() {
    let (dir, project) = make_project_with_minimal_spec();
    let secret = dir.path().parent().unwrap().join("ludwig_secret_probe.txt");
    std::fs::write(&secret, "TOP-SECRET-CONTENTS").unwrap();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let uri = format!(
        "ludwig://spec/../{}",
        secret.file_name().unwrap().to_str().unwrap()
    );
    let resp = call(&server, "resources/read", json!({ "uri": uri }));
    let _ = std::fs::remove_file(&secret);

    let leaked = resp
        .pointer("/result/contents/0/text")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        !leaked.contains("TOP-SECRET"),
        "traversal leaked an out-of-root file: {leaked:?}"
    );
    assert!(resp.pointer("/error").is_some(), "expected an error response");
}

/// Example "absolute path is rejected": a double-slash URI that decodes to an
/// absolute path must be refused, not read.
#[test]
fn resources_read_rejects_absolute_path() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(
        &server,
        "resources/read",
        json!({ "uri": "ludwig://spec//etc/hosts" }),
    );
    assert!(
        resp.pointer("/result/contents/0/text").is_none(),
        "absolute-path URI must not return file contents"
    );
    assert!(resp.pointer("/error").is_some(), "expected an error response");
}

/// {#b2}: the same confinement applies to the spec.read tool, which also takes
/// an id from the wire.
#[test]
fn spec_read_rejects_traversal_id() {
    let (dir, project) = make_project_with_minimal_spec();
    let secret = dir.path().parent().unwrap().join("ludwig_secret_probe2.txt");
    std::fs::write(&secret, "TOP-SECRET-CONTENTS").unwrap();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let id = format!("../{}", secret.file_name().unwrap().to_str().unwrap());
    let resp = call(
        &server,
        "tools/call",
        json!({ "name": "spec.read", "arguments": { "id": id } }),
    );
    let _ = std::fs::remove_file(&secret);
    // Either a JSON-RPC error, or a tool result that does not contain the secret.
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        !text.contains("TOP-SECRET"),
        "spec.read leaked an out-of-root file: {text:?}"
    );
}

/// {#b1} + example "legitimate id resolves": a normal id still works.
#[test]
fn resources_read_still_serves_legitimate_spec() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(
        &server,
        "resources/read",
        json!({ "uri": "ludwig://spec/hello-greeter" }),
    );
    let text = resp
        .pointer("/result/contents/0/text")
        .and_then(Value::as_str)
        .expect("legitimate id must still resolve");
    assert!(text.contains("## Intent"));
}

#[test]
fn unknown_method_returns_error() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(&server, "foo/bar", json!({}));
    assert_eq!(resp.pointer("/error/code"), Some(&Value::from(-32601)));
}

#[test]
fn spec_propose_returns_prompt() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(
        &server,
        "tools/call",
        json!({
            "name": "spec.propose",
            "arguments": {
                "slug": "url-shortener",
                "description": "Map long URLs to short tokens."
            }
        }),
    );
    let text = resp.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
    assert!(text.contains("url-shortener"));
    assert!(text.contains("Map long URLs"));
    assert!(text.contains("spec.write"));
}

#[test]
fn project_decompose_returns_prompt() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(
        &server,
        "tools/call",
        json!({
            "name": "project.decompose",
            "arguments": { "description": "A URL shortener with per-tenant analytics." }
        }),
    );
    let text = resp.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
    assert!(text.contains("per-tenant analytics"));
    assert!(text.contains("\"games\""));
    assert!(text.contains("\"specs\""));
}

#[test]
fn spec_write_persists_valid_draft() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project.clone()), None);
    let content = r#"---
id: from-agent
title: From agent
status: draft
owners: []
implements: []
depends_on: []
version: 1
---

## Intent
This spec was drafted by a host agent in response to a description
and then written to disk via spec.write. It exists only to exercise
that round-trip path with realistic prose.

## Behavior
- {#b1} It does the thing.

## Examples
```example name="happy"
Given a setup
When called
Then it works
```

## Invariants
- {deterministic} The thing happens.
"#;
    let resp = call(
        &server,
        "tools/call",
        json!({
            "name": "spec.write",
            "arguments": { "slug": "from-agent", "content": content }
        }),
    );
    let text = resp.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload.get("ok"), Some(&Value::Bool(true)));
    assert!(project.specs_dir().join("from-agent.spec.md").is_file());
}

#[test]
fn spec_write_rejects_invalid_and_does_not_persist() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project.clone()), None);
    let bad = "no frontmatter here\n";
    let resp = call(
        &server,
        "tools/call",
        json!({
            "name": "spec.write",
            "arguments": { "slug": "from-agent", "content": bad }
        }),
    );
    let text = resp.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload.get("ok"), Some(&Value::Bool(false)));
    assert!(!project.specs_dir().join("from-agent.spec.md").is_file());
}

#[test]
fn game_create_writes_manifest() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project.clone()), None);
    let resp = call(
        &server,
        "tools/call",
        json!({
            "name": "game.create",
            "arguments": {
                "name": "billing",
                "intent": "Per-tenant invoicing.",
                "glossary": { "Invoice": "a monthly statement" }
            }
        }),
    );
    let text = resp.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload.get("ok"), Some(&Value::Bool(true)));
    let manifest = project.specs_dir().join("billing").join("_game.md");
    assert!(manifest.is_file());
    let body = std::fs::read_to_string(&manifest).unwrap();
    assert!(body.contains("Invoice") && body.contains("monthly statement"));
}

#[test]
fn spec_diff_returns_drift_report() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(
        &server,
        "tools/call",
        json!({
            "name": "spec.diff",
            "arguments": { "id": "hello-greeter" }
        }),
    );
    let text = resp.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed.pointer("/id"), Some(&Value::String("hello-greeter".to_string())));
    // No implements: declared — files array is empty.
    assert!(parsed.pointer("/files").and_then(Value::as_array).unwrap().is_empty());
}

#[test]
fn spec_move_relocates_between_games() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project.clone()), None);
    let resp = call(
        &server,
        "tools/call",
        json!({
            "name": "spec.move",
            "arguments": { "slug": "hello-greeter", "to_game": "auth" }
        }),
    );
    let text = resp.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(true)));
    assert!(project.specs_dir().join("auth").join("hello-greeter.spec.md").is_file());
    assert!(!project.specs_dir().join("hello-greeter.spec.md").is_file());
}

#[test]
fn spec_ingest_judgments_persists_verdicts() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project.clone()), None);
    let resp = call(
        &server,
        "tools/call",
        json!({
            "name": "spec.ingest_judgments",
            "arguments": {
                "verdicts": [{
                    "invariant_key": "hello-greeter::judgment::1",
                    "verdict": "pass",
                    "rationale": "Looks good",
                    "spec_id": "hello-greeter",
                    "spec_hash": "deadbeef"
                }]
            }
        }),
    );
    let text = resp.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(true)));
    assert_eq!(parsed.get("ingested"), Some(&Value::from(1)));

    let state = project.load_state().unwrap();
    let v = state.judgments.get("hello-greeter::judgment::1").expect("verdict persisted");
    assert_eq!(v.verdict, "pass");
    assert_eq!(v.spec_hash.as_deref(), Some("deadbeef"));
}

// -- open-item fixes ---------------------------------------------------------

/// spec.list error entries must report a root-relative path, not an absolute
/// one — the success branch already does, and leaking the absolute temp/home
/// path is both inconsistent and an information leak.
#[test]
fn spec_list_error_entry_uses_relative_path() {
    let (_dir, project) = make_project_with_minimal_spec();
    // A malformed spec that fails to parse triggers the error branch.
    std::fs::write(project.specs_dir().join("broken.spec.md"), "not a spec\n").unwrap();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(
        &server,
        "tools/call",
        json!({ "name": "spec.list", "arguments": {} }),
    );
    let text = resp.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    let err_entry = parsed
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e.get("error").is_some())
        .expect("an error entry for the broken spec");
    let path = err_entry.get("path").and_then(Value::as_str).unwrap();
    assert_eq!(path, "specs/broken.spec.md", "expected a root-relative path, got {path:?}");
}

/// initialize must echo a protocol version the client requested when the server
/// supports it, and otherwise return the server's own supported version.
#[test]
fn initialize_echoes_supported_protocol_version() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);

    let supported = ludwig::mcp::PROTOCOL_VERSION;
    let resp = call(&server, "initialize", json!({ "protocolVersion": supported }));
    assert_eq!(
        resp.pointer("/result/protocolVersion").and_then(Value::as_str),
        Some(supported),
        "server should echo a supported version the client asked for"
    );

    let resp2 = call(&server, "initialize", json!({ "protocolVersion": "1999-01-01" }));
    assert_eq!(
        resp2.pointer("/result/protocolVersion").and_then(Value::as_str),
        Some(supported),
        "for an unsupported request the server returns its own version"
    );
}

/// spec.ingest_judgments should still persist all verdicts (data preservation)
/// but flag keys that match no known judgment invariant so the agent can fix a
/// typo'd key instead of having it silently stay pending forever.
#[test]
fn ingest_judgments_flags_unknown_keys() {
    let (_dir, project) = make_project_with_minimal_spec();
    // A spec carrying one judgment invariant → valid key `judged::judgment::1`.
    let judged = r#"---
id: judged
title: Judged
status: draft
owners: []
implements: []
depends_on: []
version: 1
---

## Intent
A spec that exists only to carry a judgment invariant so the ingest path has a
real key to validate against. It does nothing else of interest beyond that.

## Behavior
- {#b1} It carries a judgment invariant.

## Examples
```example name="happy"
Given a setup
When called
Then it works
```

## Invariants
- {judgment} Errors are explained in plain English.
"#;
    std::fs::write(project.specs_dir().join("judged.spec.md"), judged).unwrap();
    let server = ludwig::mcp::Server::new(Some(project.clone()), None);
    let resp = call(
        &server,
        "tools/call",
        json!({
            "name": "spec.ingest_judgments",
            "arguments": {
                "verdicts": [
                    { "invariant_key": "judged::judgment::1", "verdict": "pass" },
                    { "invariant_key": "judged::judgment::99", "verdict": "pass" }
                ]
            }
        }),
    );
    let text = resp.pointer("/result/content/0/text").and_then(Value::as_str).unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(true)));
    let unknown: Vec<String> = parsed
        .get("unknown")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    assert_eq!(unknown, vec!["judged::judgment::99".to_string()]);
    // Both are still persisted.
    let state = project.load_state().unwrap();
    assert!(state.judgments.contains_key("judged::judgment::1"));
    assert!(state.judgments.contains_key("judged::judgment::99"));
}
