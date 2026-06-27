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
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("name")?.as_str().map(String::from))
                .collect()
        })
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
        assert!(
            names.iter().any(|n| n == expected),
            "missing tool: {expected}"
        );
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
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
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
    let text = resp
        .pointer("/result/contents/0/text")
        .and_then(Value::as_str)
        .unwrap();
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
    assert!(
        resp.pointer("/error").is_some(),
        "expected an error response"
    );
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
    assert!(
        resp.pointer("/error").is_some(),
        "expected an error response"
    );
}

/// {#b2}: the same confinement applies to the spec.read tool, which also takes
/// an id from the wire.
#[test]
fn spec_read_rejects_traversal_id() {
    let (dir, project) = make_project_with_minimal_spec();
    let secret = dir
        .path()
        .parent()
        .unwrap()
        .join("ludwig_secret_probe2.txt");
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
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
    assert!(text.contains("url-shortener"));
    assert!(text.contains("Map long URLs"));
    assert!(text.contains("spec.write"));
}

#[test]
fn spec_propose_rejects_escaping_game() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    // `game` flows into `specs_dir().join(game)`; a traversal must be rejected as
    // invalid params rather than silently enumerating files outside the specs dir.
    let resp = call(
        &server,
        "tools/call",
        json!({
            "name": "spec.propose",
            "arguments": {
                "slug": "url-shortener",
                "description": "Map long URLs to short tokens.",
                "game": "../../etc"
            }
        }),
    );
    let msg = resp
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected a JSON-RPC error, got: {resp}"));
    assert!(msg.contains("kebab-case"), "got: {msg}");
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
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
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
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
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
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
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
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
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
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        parsed.pointer("/id"),
        Some(&Value::String("hello-greeter".to_string()))
    );
    // No implements: declared — files array is empty.
    assert!(
        parsed
            .pointer("/files")
            .and_then(Value::as_array)
            .unwrap()
            .is_empty()
    );
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
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(true)));
    assert!(
        project
            .specs_dir()
            .join("auth")
            .join("hello-greeter.spec.md")
            .is_file()
    );
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
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(true)));
    assert_eq!(parsed.get("ingested"), Some(&Value::from(1)));

    let state = project.load_state().unwrap();
    let v = state
        .judgments
        .get("hello-greeter::judgment::1")
        .expect("verdict persisted");
    assert_eq!(v.verdict, ludwig::project::Verdict::Pass);
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
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    let err_entry = parsed
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e.get("error").is_some())
        .expect("an error entry for the broken spec");
    let path = err_entry.get("path").and_then(Value::as_str).unwrap();
    assert_eq!(
        path, "specs/broken.spec.md",
        "expected a root-relative path, got {path:?}"
    );
}

/// initialize must echo a protocol version the client requested when the server
/// supports it, and otherwise return the server's own supported version.
#[test]
fn initialize_echoes_supported_protocol_version() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);

    let supported = ludwig::mcp::PROTOCOL_VERSION;
    let resp = call(
        &server,
        "initialize",
        json!({ "protocolVersion": supported }),
    );
    assert_eq!(
        resp.pointer("/result/protocolVersion")
            .and_then(Value::as_str),
        Some(supported),
        "server should echo a supported version the client asked for"
    );

    let resp2 = call(
        &server,
        "initialize",
        json!({ "protocolVersion": "1999-01-01" }),
    );
    assert_eq!(
        resp2
            .pointer("/result/protocolVersion")
            .and_then(Value::as_str),
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
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed.get("ok"), Some(&Value::Bool(true)));
    let unknown: Vec<String> = parsed
        .get("unknown")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(unknown, vec!["judged::judgment::99".to_string()]);
    // Both are still persisted.
    let state = project.load_state().unwrap();
    assert!(state.judgments.contains_key("judged::judgment::1"));
    assert!(state.judgments.contains_key("judged::judgment::99"));
}

// -- path traversal -----------------------------------------------

#[test]
fn spec_write_rejects_traversal_game() {
    let (dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project.clone()), None);
    let content = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/specs/valid/minimal.spec.md"
    ))
    .unwrap();
    let resp = call(
        &server,
        "tools/call",
        json!({
            "name": "spec.write",
            // The slug matches the fixture id; the malicious payload is `game`.
            "arguments": { "slug": "hello-greeter", "content": content, "game": "../../escape" }
        }),
    );
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload.get("ok"), Some(&Value::Bool(false)));
    // Nothing was written outside the specs directory.
    assert!(
        !dir.path()
            .parent()
            .unwrap()
            .join("escape")
            .join("hello-greeter.spec.md")
            .is_file()
    );
}

#[test]
fn spec_move_rejects_traversal_to_game() {
    let (dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project.clone()), None);
    let resp = call(
        &server,
        "tools/call",
        json!({
            "name": "spec.move",
            "arguments": { "slug": "hello-greeter", "to_game": "../../escape" }
        }),
    );
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload.get("ok"), Some(&Value::Bool(false)));
    // Source spec is untouched and nothing escaped the project root.
    assert!(project.specs_dir().join("minimal.spec.md").is_file());
    assert!(
        !dir.path()
            .parent()
            .unwrap()
            .join("escape")
            .join("hello-greeter.spec.md")
            .is_file()
    );
}

// -- JSON-RPC framing  ---------------------------------------------

#[test]
fn missing_method_is_invalid_request_not_parse_error() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    // Structurally valid JSON, but no `method` member.
    let resp = server
        .handle_line(r#"{"jsonrpc":"2.0","id":7}"#)
        .expect("response");
    let value = serde_json::to_value(&resp).unwrap();
    assert_eq!(value.pointer("/error/code"), Some(&Value::from(-32600)));
    // The request id is echoed back, per JSON-RPC 2.0.
    assert_eq!(value.pointer("/id"), Some(&Value::from(7)));
}

#[test]
fn non_object_message_is_invalid_request() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = server.handle_line("42").expect("response");
    let value = serde_json::to_value(&resp).unwrap();
    assert_eq!(value.pointer("/error/code"), Some(&Value::from(-32600)));
}

#[test]
fn explicit_null_id_request_gets_a_response() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    // `id: null` is present, so a response is required (not a notification).
    let resp = server
        .handle_line(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#)
        .expect("a request with explicit null id must be answered");
    let value = serde_json::to_value(&resp).unwrap();
    assert!(value.pointer("/result").is_some());
}

#[test]
fn notification_without_id_gets_no_response() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    // No `id` member at all → a notification → no response.
    assert!(
        server
            .handle_line(r#"{"jsonrpc":"2.0","method":"ping"}"#)
            .is_none()
    );
}

// -- tools/call error contract  ------------------------------------

#[test]
fn tool_call_success_is_not_flagged_as_error() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(
        &server,
        "tools/call",
        json!({ "name": "spec.list", "arguments": {} }),
    );
    assert_eq!(resp.pointer("/result/isError"), Some(&Value::Bool(false)));
}

#[test]
fn tool_execution_failure_surfaces_as_is_error_result() {
    // A Server pointed at a directory that is not a Ludwig project: the tool
    // runs but fails (no project). That is an execution failure, so it must come
    // back as a tool result with isError:true — not a JSON-RPC transport error.
    let dir = TempDir::new("ludwig-not-a-project");
    let server = ludwig::mcp::Server::new(None, Some(dir.path().to_path_buf()));
    let resp = call(
        &server,
        "tools/call",
        json!({ "name": "spec.list", "arguments": {} }),
    );
    assert!(
        resp.pointer("/error").is_none(),
        "must not be a JSON-RPC error"
    );
    assert_eq!(resp.pointer("/result/isError"), Some(&Value::Bool(true)));
    // The originating JSON-RPC code (-32001 "no project") is preserved on the
    // result so a client that branches on it still can.
    assert_eq!(
        resp.pointer("/result/code").and_then(Value::as_i64),
        Some(-32001)
    );
}

#[test]
fn malformed_tool_params_stay_json_rpc_errors() {
    // Missing required `id` argument is a params problem (-32602) and should
    // remain a protocol-level JSON-RPC error, not an isError tool result.
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(
        &server,
        "tools/call",
        json!({ "name": "spec.read", "arguments": {} }),
    );
    assert_eq!(resp.pointer("/error/code"), Some(&Value::from(-32602)));
}

// -- exec lockdown (--no-exec) -----------------------------------------------
// `spec.verify` shells out to `cargo test` (arbitrary code execution). When the
// server is started locked down it must vanish from tools/list and be refused.

#[test]
fn no_exec_hides_verify_from_tools_list() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None).with_exec(false);
    let resp = call(&server, "tools/list", json!({}));
    let names: Vec<String> = resp
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("name")?.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !names.iter().any(|n| n == "spec.verify"),
        "spec.verify must be hidden under --no-exec, got {names:?}"
    );
    // Read-only tools stay available.
    assert!(names.iter().any(|n| n == "spec.list"));
    assert!(names.iter().any(|n| n == "spec.plan"));
}

#[test]
fn no_exec_refuses_verify_call() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None).with_exec(false);
    let resp = call(
        &server,
        "tools/call",
        json!({ "name": "spec.verify", "arguments": { "id": "hello-greeter" } }),
    );
    assert_eq!(
        resp.pointer("/error/code"),
        Some(&Value::from(-32602)),
        "locked-down spec.verify must be refused as invalid params"
    );
}

#[test]
fn default_server_still_advertises_verify() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let resp = call(&server, "tools/list", json!({}));
    let names: Vec<String> = resp
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("name")?.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert!(names.iter().any(|n| n == "spec.verify"));
}

// -- implements: path confinement --------------------------------------------
// `implements:` globs are spec-controlled and a spec can be written via the
// untrusted spec.write tool. A pattern that escapes the project tree must be
// rejected at write time so verify/drift can never read outside the project.

#[test]
fn spec_write_rejects_escaping_implements() {
    let (_dir, project) = make_project_with_minimal_spec();
    let server = ludwig::mcp::Server::new(Some(project), None);
    let content = "---\n\
id: escaper\n\
title: Escaper\n\
status: draft\n\
owners: []\n\
implements:\n\
  - ../../../../etc/passwd\n\
depends_on: []\n\
version: 1\n\
---\n\
\n\
## Intent\n\
A spec that tries to declare an implements path escaping the project root, \
which the validator must reject before it is ever persisted to disk anywhere.\n\
\n\
## Behavior\n\
- {#b1} does a thing\n\
\n\
## Examples\n\
```example name=\"happy\"\n\
Given a thing\n\
When it runs\n\
Then it works\n\
```\n\
\n\
## Invariants\n\
- {deterministic} something holds.\n";
    let resp = call(
        &server,
        "tools/call",
        json!({ "name": "spec.write", "arguments": { "slug": "escaper", "content": content } }),
    );
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or("");
    let parsed: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    assert_eq!(
        parsed.get("ok"),
        Some(&Value::Bool(false)),
        "escaping implements must be rejected: {text}"
    );
    // And nothing was written.
    assert!(
        ludwig::project::Project::open(_dir.path())
            .unwrap()
            .find_spec_by_id("escaper")
            .is_none(),
        "rejected spec must not be persisted"
    );
}
