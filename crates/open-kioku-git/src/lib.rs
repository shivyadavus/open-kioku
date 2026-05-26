use open_kioku_errors::{OkError, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub fn discover_root(start: impl AsRef<Path>) -> Result<PathBuf> {
    let mut current = start.as_ref().canonicalize()?;
    loop {
        if current.join(".git").exists() || current.join("ok.toml").exists() {
            return Ok(current);
        }
        if !current.pop() {
            return Ok(start.as_ref().canonicalize()?);
        }
    }
}

pub fn branch(root: impl AsRef<Path>) -> Option<String> {
    let head = fs::read_to_string(root.as_ref().join(".git/HEAD")).ok()?;
    if let Some(value) = head.strip_prefix("ref: refs/heads/") {
        return Some(value.trim().to_string());
    }
    None
}

pub fn commit(root: impl AsRef<Path>) -> Option<String> {
    let head = fs::read_to_string(root.as_ref().join(".git/HEAD")).ok()?;
    if !head.starts_with("ref: ") {
        return Some(head.trim().to_string());
    }
    let reference = head.trim().strip_prefix("ref: ")?;
    fs::read_to_string(root.as_ref().join(".git").join(reference))
        .ok()
        .map(|value| value.trim().to_string())
}

pub fn require_repo(root: impl AsRef<Path>) -> Result<PathBuf> {
    let root = discover_root(root)?;
    if !root.exists() {
        return Err(OkError::Repository(format!(
            "repository root does not exist: {}",
            root.display()
        )));
    }
    Ok(root)
}
