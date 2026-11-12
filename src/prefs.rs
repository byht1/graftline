//! UI preferences of the desktop app (theme, open tabs, panel sizes), kept in `ui-prefs.json`
//! as a map of `gl.*` keys to JSON text.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Upper limit for all preferences together (keys and values).
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
const PREFIX: &str = "gl.";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Prefs {
    /// Increases with each save.
    pub rev: u64,
    /// Key -> JSON text of the value.
    pub items: BTreeMap<String, String>,
}

pub fn store_path(data_dir: &Path) -> PathBuf {
    data_dir.join("ui-prefs.json")
}

pub fn load(path: &Path) -> Prefs {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Writes a temporary file, then renames it: a crash does not leave a half-written file.
pub fn write_atomic(path: &Path, content: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("Cannot create {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content).with_context(|| format!("Cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("Cannot write {}", path.display()))
}

pub fn save(path: &Path, prefs: &Prefs) -> Result<()> {
    write_atomic(path, &serde_json::to_string(prefs)?)
}

/// Keeps only `gl.*` keys and checks the total size. Returns the error message for the user.
pub fn check(items: BTreeMap<String, String>) -> Result<BTreeMap<String, String>, String> {
    let items: BTreeMap<_, _> = items.into_iter().filter(|(k, _)| k.starts_with(PREFIX)).collect();
    let size: usize = items.iter().map(|(k, v)| k.len() + v.len()).sum();
    if size > MAX_BYTES {
        return Err(format!("UI preferences are too large ({} KB)", size / 1024));
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = store_path(dir.path());
        let mut p = Prefs { rev: 3, ..Default::default() };
        p.items.insert("gl.theme".into(), "\"one-dark-pro\"".into());
        save(&path, &p).unwrap();
        assert_eq!(load(&path), p);
    }

    #[test]
    fn checks_keys_and_size() {
        let mut items = BTreeMap::new();
        items.insert("gl.theme".to_string(), "x".to_string());
        items.insert("other".to_string(), "y".to_string());
        assert_eq!(check(items).unwrap().len(), 1);
        let mut big = BTreeMap::new();
        big.insert("gl.big".to_string(), "x".repeat(MAX_BYTES + 1));
        assert!(check(big).is_err());
    }
}
