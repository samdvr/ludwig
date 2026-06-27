use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;

use crate::error::ParseError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Draft,
    Active,
    Deprecated,
}

impl Status {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "active" => Some(Self::Active),
            "deprecated" => Some(Self::Deprecated),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Deprecated => "deprecated",
        }
    }
}

const KNOWN_FIELDS: &[&str] =
    &["id", "title", "status", "owners", "implements", "depends_on", "version", "hash"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Frontmatter {
    pub id: String,
    pub title: String,
    pub status: Status,
    pub owners: Vec<String>,
    pub implements: Vec<String>,
    pub depends_on: Vec<String>,
    pub version: u32,
    pub hash: Option<String>,
}

impl Frontmatter {
    pub fn is_draft(&self) -> bool { self.status == Status::Draft }
    pub fn is_active(&self) -> bool { self.status == Status::Active }

    pub fn from_yaml(yaml: &str, source: Option<&Path>) -> Result<Self, ParseError> {
        let raw: YamlValue = serde_yaml::from_str(yaml)
            .map_err(|e| ParseError::at(source, format!("frontmatter YAML invalid: {e}")))?;

        let map = match raw {
            YamlValue::Mapping(m) => m,
            YamlValue::Null => {
                return Err(ParseError::at(source, "frontmatter must be a mapping"));
            }
            _ => return Err(ParseError::at(source, "frontmatter must be a mapping")),
        };

        // Reject unknown keys.
        let mut entries: BTreeMap<String, YamlValue> = BTreeMap::new();
        for (k, v) in map {
            let key = match k {
                YamlValue::String(s) => s,
                _ => {
                    return Err(ParseError::at(
                        source,
                        "frontmatter keys must be strings",
                    ));
                }
            };
            if !KNOWN_FIELDS.contains(&key.as_str()) {
                return Err(ParseError::at(
                    source,
                    format!("unknown frontmatter key: {key}"),
                ));
            }
            entries.insert(key, v);
        }

        let id = require_string(&entries, "id", source)?;
        // The `id` is interpolated into filesystem paths (generated test file,
        // cache snapshot) and the judgment-key namespace. Validate its shape at
        // the parse boundary — the single trust boundary for specs that arrive
        // hand-written or persisted by an older binary — so a `..`, an absolute
        // path, or other separators can never reach those paths. See `is_valid_slug`.
        if !crate::util::is_valid_slug(&id) {
            return Err(ParseError::at(
                source,
                format!(
                    "frontmatter `id` must be a kebab-case slug (lowercase letters, digits, \
                     and dashes; `/` allowed for sub-games): {id:?}"
                ),
            ));
        }
        let title = require_string(&entries, "title", source)?;
        let status_str = require_string(&entries, "status", source)?;
        let status = Status::parse(&status_str).ok_or_else(|| {
            ParseError::at(
                source,
                format!(
                    "frontmatter `status` must be one of draft|active|deprecated, got {status_str:?}"
                ),
            )
        })?;
        let version_i = require_int(&entries, "version", source)?;
        if version_i < 1 {
            return Err(ParseError::at(source, "frontmatter `version` must be >= 1"));
        }
        if version_i > u32::MAX as i64 {
            return Err(ParseError::at(
                source,
                format!("frontmatter `version` must be <= {}", u32::MAX),
            ));
        }
        let version = version_i as u32;

        let owners = optional_string_list(&entries, "owners", source)?;
        let implements = optional_string_list(&entries, "implements", source)?;
        let depends_on = optional_string_list(&entries, "depends_on", source)?;

        // `implements:` patterns are spec-controlled and a spec can arrive from
        // an untrusted MCP client. Reject any pattern that escapes the project
        // tree (absolute, drive-prefixed, or containing `..`) at validation time
        // so it can never be persisted, let alone expanded by verify/drift. See
        // spec `mcp-path-confinement`.
        for pat in &implements {
            if crate::util::pattern_escapes_root(pat) {
                return Err(ParseError::at(
                    source,
                    format!(
                        "frontmatter `implements` entry {pat:?} must be a project-relative path \
                         (no leading `/`, drive prefix, or `..` segments)"
                    ),
                ));
            }
        }

        let hash = match entries.get("hash") {
            None | Some(YamlValue::Null) => None,
            Some(YamlValue::String(s)) => Some(s.clone()),
            Some(_) => {
                return Err(ParseError::at(
                    source,
                    "frontmatter `hash` must be a string if present",
                ));
            }
        };

        Ok(Self { id, title, status, owners, implements, depends_on, version, hash })
    }

    /// Canonical form for hashing: a fixed set of fields in a fixed order, with
    /// the `hash` field omitted (it is derived from this very output). Emitted by
    /// hand rather than via `serde_yaml` so the hashed form is fully owned here
    /// and cannot shift under us when the YAML library changes its formatting — a
    /// dependency bump that re-quotes scalars or re-wraps sequences would
    /// otherwise silently invalidate every stored spec hash. The result is used
    /// only for hashing and version-diffing, never parsed back, so scalars are
    /// written verbatim and an empty list renders as `[]`.
    pub fn to_canonical_yaml(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("id: {}\n", self.id));
        out.push_str(&format!("title: {}\n", self.title));
        out.push_str(&format!("status: {}\n", self.status.as_str()));
        out.push_str(&format!("version: {}\n", self.version));
        out.push_str(&format!("owners: [{}]\n", self.owners.join(", ")));
        out.push_str(&format!("implements: [{}]\n", self.implements.join(", ")));
        out.push_str(&format!("depends_on: [{}]\n", self.depends_on.join(", ")));
        out
    }
}

fn require_string(
    entries: &BTreeMap<String, YamlValue>,
    key: &str,
    source: Option<&Path>,
) -> Result<String, ParseError> {
    match entries.get(key) {
        Some(YamlValue::String(s)) => Ok(s.clone()),
        Some(_) => Err(ParseError::at(
            source,
            format!("frontmatter `{key}` must be a string"),
        )),
        None => Err(ParseError::at(
            source,
            format!("frontmatter missing required key `{key}`"),
        )),
    }
}

fn require_int(
    entries: &BTreeMap<String, YamlValue>,
    key: &str,
    source: Option<&Path>,
) -> Result<i64, ParseError> {
    match entries.get(key) {
        Some(YamlValue::Number(n)) => n.as_i64().ok_or_else(|| {
            ParseError::at(source, format!("frontmatter `{key}` must be an integer"))
        }),
        Some(_) => Err(ParseError::at(
            source,
            format!("frontmatter `{key}` must be an Integer"),
        )),
        None => Err(ParseError::at(
            source,
            format!("frontmatter missing required key `{key}`"),
        )),
    }
}

fn optional_string_list(
    entries: &BTreeMap<String, YamlValue>,
    key: &str,
    source: Option<&Path>,
) -> Result<Vec<String>, ParseError> {
    match entries.get(key) {
        None | Some(YamlValue::Null) => Ok(Vec::new()),
        Some(YamlValue::Sequence(seq)) => {
            let mut out = Vec::with_capacity(seq.len());
            for item in seq {
                match item {
                    YamlValue::String(s) => out.push(s.clone()),
                    _ => {
                        return Err(ParseError::at(
                            source,
                            format!("frontmatter `{key}` entries must be strings"),
                        ));
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(ParseError::at(
            source,
            format!("frontmatter `{key}` must be a list"),
        )),
    }
}
