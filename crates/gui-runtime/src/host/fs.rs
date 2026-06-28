//! Filesystem effects, jailed to the surface's `state_dir`. Stateless — every op takes the jail
//! root (and the size limit, for writes); the host enforces the `fs` capability.

use std::path::{Component, Path, PathBuf};

use gui_schema::Limits;
use serde_json::Value;

use super::HostError;

pub fn read(state_dir: &Path, rel: &str) -> Result<Value, HostError> {
    let path = jail(state_dir, rel)?;
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(Value::String(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
        Err(e) => Err(HostError::Other(format!("fs read: {e}"))),
    }
}

pub fn write(state_dir: &Path, limits: &Limits, rel: &str, data: &str) -> Result<(), HostError> {
    let cap = (limits.fs_kb as usize).saturating_mul(1024);
    if data.len() > cap {
        return Err(HostError::Limit(format!("fs write exceeds {} KB", limits.fs_kb)));
    }
    let path = jail(state_dir, rel)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| HostError::Other(format!("fs mkdir: {e}")))?;
    }
    gui_schema::atomic_write(&path, data.as_bytes()).map_err(|e| HostError::Other(format!("fs write: {e}")))
}

pub fn list(state_dir: &Path, prefix: &str) -> Result<Value, HostError> {
    let dir = jail(state_dir, prefix)?;
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.filter_map(|e| e.ok()) {
            if let Some(n) = e.file_name().to_str() {
                out.push(Value::String(n.to_string()));
            }
        }
    }
    Ok(Value::Array(out))
}

/// Resolve a guest-supplied relative path inside the jail, rejecting absolute paths and any `..`
/// traversal.
fn jail(state_dir: &Path, rel: &str) -> Result<PathBuf, HostError> {
    let p = Path::new(rel);
    if p.is_absolute()
        || p.components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_) | Component::RootDir))
    {
        return Err(HostError::Denied(format!("path {rel:?} escapes state dir")));
    }
    Ok(state_dir.join(p))
}
