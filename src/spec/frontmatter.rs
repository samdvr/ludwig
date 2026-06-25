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

    /// Canonical YAML form for hashing: sorted keys, `hash` field omitted.
    pub fn to_canonical_yaml(&self) -> String {
        // Build a BTreeMap so keys serialize in sorted order. Use a single YAML mapping.
        let mut m: BTreeMap<&str, YamlValue> = BTreeMap::new();
        m.insert("id", YamlValue::String(self.id.clone()));
        m.insert("title", YamlValue::String(self.title.clone()));
        m.insert("status", YamlValue::String(self.status.as_str().to_string()));
        m.insert(
            "owners",
            YamlValue::Sequence(
                self.owners.iter().map(|s| YamlValue::String(s.clone())).collect(),
            ),
        );
        m.insert(
            "implements",
            YamlValue::Sequence(
                self.implements.iter().map(|s| YamlValue::String(s.clone())).collect(),
            ),
        );
        m.insert(
            "depends_on",
            YamlValue::Sequence(
                self.depends_on.iter().map(|s| YamlValue::String(s.clone())).collect(),
            ),
        );
        m.insert("version", YamlValue::Number(self.version.into()));

        serde_yaml::to_string(&m).unwrap_or_default()
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
