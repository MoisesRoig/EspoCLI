//! Cached view over /api/v1/Metadata (825 KB on a stock instance).
//! Everything the CLI knows about entities, field types and links comes from here.

use crate::client::{Client, usage};
use anyhow::{Context, Result};
use serde_json::{Map, Value};
use std::time::{Duration, SystemTime};

const TTL: Duration = Duration::from_secs(24 * 60 * 60);

pub struct Meta {
    root: Value,
}

pub struct FieldInfo<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub required: bool,
    pub options: Vec<&'a str>,
}

pub struct LinkInfo<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub entity: &'a str,
}

impl Meta {
    #[cfg(test)]
    pub fn from_value(root: Value) -> Self {
        Self { root }
    }

    /// Reads the cache without touching the network, for callers that must stay cheap.
    pub fn cached(profile: &str, url: &str) -> Option<(Self, SystemTime)> {
        let path = crate::config::cache_dir(profile, url).join("metadata.json");
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
        let root = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
        Some((Self { root }, modified))
    }

    pub fn load(client: &Client, profile: &str, url: &str, refresh: bool) -> Result<Self> {
        let path = crate::config::cache_dir(profile, url).join("metadata.json");
        if !refresh && fresh(&path) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(root) = serde_json::from_str(&text) {
                    return Ok(Self { root });
                }
            }
        }
        let root = client.send("GET", "Metadata", &[], None, &[])?;
        if let Some(dir) = path.parent() {
            if crate::config::create_private_dir(dir).is_ok() {
                let _ = crate::config::write_private(&path, root.to_string().as_bytes());
            }
        }
        Ok(Self { root })
    }

    fn section(&self, name: &str) -> Option<&Map<String, Value>> {
        self.root.get(name)?.as_object()
    }

    /// Accepts any casing so `espo list lead` works; errors list nothing, the entities command does.
    pub fn resolve_entity(&self, name: &str) -> Result<String> {
        let defs = self.section("entityDefs").context("metadata has no entityDefs")?;
        if defs.contains_key(name) {
            return Ok(name.to_string());
        }
        let found = defs.keys().find(|k| k.eq_ignore_ascii_case(name));
        match found {
            Some(k) => Ok(k.clone()),
            None => Err(usage(format!("unknown entity {name:?}; run: espo entities"))),
        }
    }

    pub fn fields(&self, entity: &str) -> Option<&Map<String, Value>> {
        self.root.get("entityDefs")?.get(entity)?.get("fields")?.as_object()
    }

    pub fn links(&self, entity: &str) -> Option<&Map<String, Value>> {
        self.root.get("entityDefs")?.get(entity)?.get("links")?.as_object()
    }

    pub fn field_type(&self, entity: &str, field: &str) -> Option<&str> {
        self.fields(entity)?.get(field)?.get("type")?.as_str()
    }

    pub fn has_field(&self, entity: &str, field: &str) -> bool {
        self.fields(entity).is_some_and(|f| f.contains_key(field))
    }

    /// Target entity of a link, used to pick default columns for `related`.
    pub fn link_entity(&self, entity: &str, link: &str) -> Option<&str> {
        self.links(entity)?.get(link)?.get("entity")?.as_str()
    }

    /// entity=true is the API-addressable set; object=true is the subset users see as records.
    pub fn entities(&self, only_objects: bool) -> Vec<(&str, &str, bool)> {
        let Some(scopes) = self.section("scopes") else { return Vec::new() };
        let mut out: Vec<(&str, &str, bool)> = scopes
            .iter()
            .filter(|(_, v)| v.get("entity").and_then(Value::as_bool).unwrap_or(false))
            .map(|(k, v)| {
                let object = v.get("object").and_then(Value::as_bool).unwrap_or(false);
                let module = v.get("module").and_then(Value::as_str).unwrap_or("Custom");
                (k.as_str(), module, object)
            })
            .filter(|(_, _, object)| !only_objects || *object)
            .collect();
        out.sort_unstable_by_key(|(name, _, _)| *name);
        out
    }

    pub fn field_list(&self, entity: &str) -> Vec<FieldInfo<'_>> {
        let Some(fields) = self.fields(entity) else { return Vec::new() };
        let mut out: Vec<FieldInfo<'_>> = fields
            .iter()
            .filter(|(_, def)| !def.get("disabled").and_then(Value::as_bool).unwrap_or(false))
            .map(|(name, def)| FieldInfo {
                name,
                kind: def.get("type").and_then(Value::as_str).unwrap_or("?"),
                required: def.get("required").and_then(Value::as_bool).unwrap_or(false),
                options: def
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default(),
            })
            .collect();
        out.sort_unstable_by_key(|f| f.name);
        out
    }

    pub fn link_list(&self, entity: &str) -> Vec<LinkInfo<'_>> {
        let Some(links) = self.links(entity) else { return Vec::new() };
        let mut out: Vec<LinkInfo<'_>> = links
            .iter()
            .map(|(name, def)| LinkInfo {
                name,
                kind: def.get("type").and_then(Value::as_str).unwrap_or("?"),
                entity: def.get("entity").and_then(Value::as_str).unwrap_or("?"),
            })
            .collect();
        out.sort_unstable_by_key(|l| l.name);
        out
    }
}

fn fresh(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < TTL)
}
