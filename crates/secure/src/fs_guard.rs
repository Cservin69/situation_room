//! Filesystem guard — prevents writes outside a designated workspace root.
//!
//! Used by the article-archiving feature and anywhere else the app
//! persists data derived from user input. Protects against classic
//! path-traversal: an attacker-controlled filename like
//! `../../../../../etc/cron.d/evil` gets rejected.
//!
//! ## Model
//!
//! You construct an [`FsGuard`] rooted at an absolute path. The guard
//! resolves any input path relative to the root, canonicalizes it, and
//! verifies the canonical path is still within the root. Symlinks that
//! escape the root are rejected (but symlinks *inside* the root are fine).

use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FsViolation {
    #[error("path is not absolute: {0}")]
    NotAbsolute(String),
    #[error("path escapes workspace root: {0}")]
    EscapesRoot(String),
    #[error("path contains null byte")]
    NullByte,
    #[error("io error: {0}")]
    Io(String),
}

pub struct FsGuard {
    root: PathBuf,
}

impl FsGuard {
    /// Create a guard rooted at `root`. `root` must be an absolute path
    /// that exists.
    pub fn new(root: PathBuf) -> Result<Self, FsViolation> {
        if !root.is_absolute() {
            return Err(FsViolation::NotAbsolute(root.display().to_string()));
        }
        // Canonicalize so later comparisons resolve symlinks in the root itself.
        let root = root.canonicalize().map_err(|e| FsViolation::Io(e.to_string()))?;
        Ok(Self { root })
    }

    /// Resolve a user-supplied relative path against the root and verify
    /// the result stays within the root. Does NOT create files.
    pub fn resolve(&self, user_path: &Path) -> Result<PathBuf, FsViolation> {
        // Reject null bytes (defense-in-depth for C-string boundaries)
        if user_path.to_string_lossy().contains('\0') {
            return Err(FsViolation::NullByte);
        }

        // Build candidate path by joining components, stripping any attempt
        // to go up or to an absolute path.
        let mut candidate = self.root.clone();
        for comp in user_path.components() {
            match comp {
                Component::Normal(part) => candidate.push(part),
                Component::CurDir => {} // ./ is harmless
                Component::ParentDir => {
                    return Err(FsViolation::EscapesRoot(user_path.display().to_string()));
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(FsViolation::EscapesRoot(user_path.display().to_string()));
                }
            }
        }

        // If the candidate exists, canonicalize and re-check containment
        // (handles symlink escape). If it doesn't exist yet, canonicalize
        // its parent and append the leaf — the parent must already be safe.
        let verified = if candidate.exists() {
            candidate
                .canonicalize()
                .map_err(|e| FsViolation::Io(e.to_string()))?
        } else {
            let parent = candidate
                .parent()
                .ok_or_else(|| FsViolation::EscapesRoot(candidate.display().to_string()))?;
            let parent_canon = parent
                .canonicalize()
                .map_err(|e| FsViolation::Io(e.to_string()))?;
            let leaf = candidate
                .file_name()
                .ok_or_else(|| FsViolation::EscapesRoot(candidate.display().to_string()))?;
            parent_canon.join(leaf)
        };

        if !verified.starts_with(&self.root) {
            return Err(FsViolation::EscapesRoot(verified.display().to_string()));
        }

        Ok(verified)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_root() -> PathBuf {
        let base = std::env::temp_dir().join(format!("fs_guard_test_{}", std::process::id()));
        if base.exists() { let _ = fs::remove_dir_all(&base); }
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn rejects_parent_traversal() {
        let root = temp_root();
        let guard = FsGuard::new(root.clone()).unwrap();
        let bad = Path::new("../etc/passwd");
        assert!(matches!(guard.resolve(bad), Err(FsViolation::EscapesRoot(_))));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn accepts_plain_filename() {
        let root = temp_root();
        let guard = FsGuard::new(root.clone()).unwrap();
        let ok = Path::new("article_123.html");
        let resolved = guard.resolve(ok).unwrap();
        assert!(resolved.starts_with(&root));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_absolute_path() {
        let root = temp_root();
        let guard = FsGuard::new(root.clone()).unwrap();
        let bad = Path::new("/etc/passwd");
        assert!(matches!(guard.resolve(bad), Err(FsViolation::EscapesRoot(_))));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_null_byte() {
        let root = temp_root();
        let guard = FsGuard::new(root.clone()).unwrap();
        let bad = Path::new("file\0name");
        assert!(matches!(guard.resolve(bad), Err(FsViolation::NullByte)));
        fs::remove_dir_all(&root).ok();
    }
}
