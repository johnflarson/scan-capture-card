//! Small JSON file cache in `MTG_SCAN_CACHE_DIR` when set, else the user's
//! cache folder (`%LOCALAPPDATA%\mtg-scan` on Windows, `~/.cache/mtg-scan`
//! on Linux).

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub fn dir() -> PathBuf {
    crate::config::env_path("MTG_SCAN_CACHE_DIR").unwrap_or_else(|| {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("mtg-scan")
    })
}

pub fn path(name: &str) -> PathBuf {
    dir().join(name)
}

/// Read `name` if it exists and was written less than `max_age` ago.
pub fn read_fresh<T: DeserializeOwned>(name: &str, max_age: Duration) -> Option<T> {
    let p = path(name);
    let modified = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default();
    if age > max_age {
        return None;
    }
    read_any(name)
}

/// Read `name` regardless of age (used as a fallback when a refresh fails).
pub fn read_any<T: DeserializeOwned>(name: &str) -> Option<T> {
    let bytes = std::fs::read(path(name)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn write<T: Serialize>(name: &str, value: &T) -> Result<()> {
    let p = path(name);
    std::fs::create_dir_all(dir()).context("failed to create cache directory")?;
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(value)?)
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, &p).with_context(|| format!("failed to write {}", p.display()))?;
    Ok(())
}
