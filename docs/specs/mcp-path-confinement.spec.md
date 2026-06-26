---
id: mcp-path-confinement
title: MCP spec lookups are confined to the project
status: active
owners: []
implements:
  - src/project.rs
  - src/mcp.rs
depends_on: []
version: 1
---

## Intent
The MCP server resolves spec ids and `ludwig://spec/<id>` resource URIs that
arrive directly from an LLM client. A client may be steered by untrusted text,
so a spec lookup must never become a primitive for reading files outside the
project. Lookups by id must resolve only to `*.spec.md` files inside the
project's specs directory; anything that escapes it is rejected, not read.

## Behavior
- {#b1} A bare spec id resolves only to a `*.spec.md` file located under the project's specs directory.
- {#b2} An id containing path separators, parent-directory segments (`..`), or an absolute path is rejected by the MCP layer rather than resolved against the filesystem.
- {#b3} A rejected lookup yields an MCP "no such spec" / invalid-params error and reads no file off disk.
- {#b4} The CLI's existing convenience of passing a relative path to a spec is unaffected, because confinement is enforced at the MCP boundary, not in the CLI.

## Examples
```example name="legitimate id resolves"
Given a project whose specs directory contains login.spec.md with id "login"
When the MCP server reads resource ludwig://spec/login
Then it returns the markdown of login.spec.md
```

```example name="parent traversal is rejected"
Given a project and a secret file outside the project root
When the MCP server reads resource ludwig://spec/../../secret.txt
Then no file outside the project is read
And the server returns a "no such spec" error
```

```example name="absolute path is rejected"
Given a project
When the MCP server reads resource ludwig://spec//etc/hosts
Then the server returns a "no such spec" error rather than the file contents
```

## Invariants
- {deterministic} Any spec id accepted by the MCP layer maps to a path whose canonicalized location is a descendant of the project specs directory.
- {deterministic} For an id containing "..", "/", or a leading separator, the MCP read returns an error and performs no read of a path outside the specs directory.
- {judgment} The rejection error message tells the agent the id is unknown without echoing the absolute path it probed.
