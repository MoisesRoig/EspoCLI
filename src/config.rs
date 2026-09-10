//! Named profiles persisted as TOML under the XDG config dir, mode 600.

use crate::client::usage;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Default, Serialize, Deserialize)]
pub struct Config {
    pub default: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub url: String,
    pub api_key: String,
}

fn base_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var).map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback),
    }
}

pub fn config_path() -> PathBuf {
    base_dir("XDG_CONFIG_HOME", ".config").join("espo").join("config.toml")
}

/// Keyed by profile *and* instance: env credentials reuse the name "env", and a profile can
/// be repointed, so the URL has to take part or a stale schema would be served for it.
pub fn cache_dir(profile: &str, url: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.trim_end_matches('/').hash(&mut h);
    let key = format!("{profile}-{:08x}", h.finish() as u32);
    base_dir("XDG_CACHE_HOME", ".cache").join("espo").join(key)
}

/// Profile names become path components, so anything but this set could escape the cache dir.
pub fn check_profile_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(usage(format!(
            "invalid profile name {name:?}; use letters, digits, '-', '_' or '.' (max 64)"
        )))
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Writes atomically through a temp file so an interrupted save cannot truncate the config.
    pub fn save(&self) -> Result<()> {
        let path = config_path();
        let dir = path.parent().context("config path has no parent")?;
        create_private_dir(dir)?;
        let tmp = path.with_extension("toml.tmp");
        let body = toml::to_string_pretty(self)?;
        write_private(&tmp, body.as_bytes())?;
        std::fs::rename(&tmp, &path).with_context(|| format!("renaming into {}", path.display()))
    }

    /// Environment credentials win over the file; otherwise --profile, then $ESPO_PROFILE, then default.
    pub fn resolve(&self, requested: Option<&str>) -> Result<(String, Profile)> {
        if let (Ok(url), Ok(api_key)) = (std::env::var("ESPO_URL"), std::env::var("ESPO_API_KEY")) {
            if !url.is_empty() && !api_key.is_empty() {
                return Ok(("env".into(), Profile { url, api_key }));
            }
        }
        let name = requested
            .map(str::to_owned)
            .or_else(|| std::env::var("ESPO_PROFILE").ok().filter(|s| !s.is_empty()))
            .or_else(|| self.default.clone())
            .unwrap_or_else(|| "default".into());
        check_profile_name(&name)?;
        match self.profiles.get(&name) {
            Some(p) => Ok((name, p.clone())),
            None if self.profiles.is_empty() => {
                Err(usage("no profile configured; run: espo auth login --url <url> --api-key <key>"))
            }
            None => Err(usage(format!(
                "unknown profile {name:?}; available: {}",
                self.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
            ))),
        }
    }
}

/// Created 600 from the start: a chmod after the write leaves the secret briefly readable.
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).with_context(|| format!("creating {}", path.display()))?;
    f.write_all(bytes).with_context(|| format!("writing {}", path.display()))?;
    // An existing file keeps its old mode, so enforce it explicitly too.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting mode 600 on {}", path.display()))?;
    }
    Ok(())
}

pub fn create_private_dir(dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("setting mode 700 on {}", dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::cache_dir;
    use super::check_profile_name as check;

    #[test]
    fn cache_is_keyed_by_instance_not_only_by_profile() {
        let a = cache_dir("env", "https://one.example.com");
        let b = cache_dir("env", "https://two.example.com");
        assert_ne!(a, b, "two instances must not share a metadata cache");
        // A trailing slash is the same instance.
        assert_eq!(a, cache_dir("env", "https://one.example.com/"));
        // Same instance, different profile, separate cache: ACL can differ per API user.
        assert_ne!(a, cache_dir("prod", "https://one.example.com"));
    }

    #[test]
    fn profile_names_cannot_escape_the_cache_dir() {
        assert!(check("prod").is_ok());
        assert!(check("staging-2.eu_1").is_ok());
        assert!(check("../../pwned").is_err());
        assert!(check("a/b").is_err());
        assert!(check("..").is_err());
        assert!(check(".").is_err());
        assert!(check("").is_err());
        assert!(check(&"x".repeat(65)).is_err());
        assert!(check("na\0me").is_err());
    }
}
