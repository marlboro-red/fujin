use crate::artifact::{ArtifactSet, FileChange, FileChangeKind};
use crate::error::{CoreError, CoreResult};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// A snapshot of the workspace filesystem at a point in time.
/// Maps relative paths to their SHA-256 hashes.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceSnapshot {
    pub files: HashMap<PathBuf, FileEntry>,
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub hash: String,
    pub size: u64,
}

/// Manages workspace directory operations: snapshots, diffs, file listing.
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Create a new Workspace manager for the given root directory.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Ensure the workspace directory exists.
    pub fn ensure_exists(&self) -> CoreResult<()> {
        if !self.root.exists() {
            std::fs::create_dir_all(&self.root).map_err(|e| CoreError::WorkspaceError {
                path: self.root.clone(),
                message: format!("Failed to create workspace directory: {e}"),
            })?;
        }
        Ok(())
    }

    /// Get the root path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Take a snapshot of all files in the workspace.
    pub fn snapshot(&self) -> CoreResult<WorkspaceSnapshot> {
        let mut files = HashMap::new();

        if !self.root.exists() {
            return Ok(WorkspaceSnapshot { files });
        }

        for entry in WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !is_hidden(e))
        {
            let entry = entry.map_err(|e| CoreError::WorkspaceError {
                path: self.root.clone(),
                message: format!("Failed to walk workspace: {e}"),
            })?;

            if !entry.file_type().is_file() {
                continue;
            }

            let abs_path = entry.path();
            let rel_path = abs_path.strip_prefix(&self.root).map_err(|e| {
                CoreError::WorkspaceError {
                    path: abs_path.to_path_buf(),
                    message: format!("Failed to compute relative path: {e}"),
                }
            })?;

            let content =
                std::fs::read(abs_path).map_err(|e| CoreError::WorkspaceError {
                    path: abs_path.to_path_buf(),
                    message: format!("Failed to read file: {e}"),
                })?;

            let hash = format!("{:x}", Sha256::digest(&content));
            let size = content.len() as u64;

            files.insert(rel_path.to_path_buf(), FileEntry { hash, size });
        }

        Ok(WorkspaceSnapshot { files })
    }

    /// Diff two snapshots to produce an ArtifactSet.
    pub fn diff(before: &WorkspaceSnapshot, after: &WorkspaceSnapshot) -> ArtifactSet {
        let mut changes = Vec::new();

        // Check for created and modified files
        for (path, after_entry) in &after.files {
            match before.files.get(path) {
                None => {
                    changes.push(FileChange {
                        path: path.clone(),
                        kind: FileChangeKind::Created,
                        hash: Some(after_entry.hash.clone()),
                        size: Some(after_entry.size),
                    });
                }
                Some(before_entry) if before_entry.hash != after_entry.hash => {
                    changes.push(FileChange {
                        path: path.clone(),
                        kind: FileChangeKind::Modified,
                        hash: Some(after_entry.hash.clone()),
                        size: Some(after_entry.size),
                    });
                }
                _ => {} // unchanged
            }
        }

        // Check for deleted files
        for path in before.files.keys() {
            if !after.files.contains_key(path) {
                changes.push(FileChange {
                    path: path.clone(),
                    kind: FileChangeKind::Deleted,
                    hash: None,
                    size: None,
                });
            }
        }

        // Sort for deterministic output
        changes.sort_by(|a, b| a.path.cmp(&b.path));

        ArtifactSet { changes }
    }

    /// List all files in the workspace (relative paths).
    pub fn list_files(&self) -> CoreResult<Vec<PathBuf>> {
        let mut files = Vec::new();

        if !self.root.exists() {
            return Ok(files);
        }

        for entry in WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !is_hidden(e))
        {
            let entry = entry.map_err(|e| CoreError::WorkspaceError {
                path: self.root.clone(),
                message: format!("Failed to walk workspace: {e}"),
            })?;

            if entry.file_type().is_file() {
                let rel_path = entry.path().strip_prefix(&self.root).map_err(|e| {
                    CoreError::WorkspaceError {
                        path: entry.path().to_path_buf(),
                        message: format!("Failed to compute relative path: {e}"),
                    }
                })?;
                files.push(rel_path.to_path_buf());
            }
        }

        files.sort();
        Ok(files)
    }

    /// Read a file from the workspace.
    pub fn read_file(&self, rel_path: &Path) -> CoreResult<String> {
        let abs_path = self.root.join(rel_path);
        std::fs::read_to_string(&abs_path).map_err(|e| CoreError::WorkspaceError {
            path: abs_path,
            message: format!("Failed to read file: {e}"),
        })
    }
}

/// Skip hidden files and directories (starting with '.').
fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_snapshot_empty_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path().to_path_buf());
        let snap = ws.snapshot().unwrap();
        assert!(snap.files.is_empty());
    }

    #[test]
    fn test_snapshot_and_diff() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path().to_path_buf());

        // Create a file
        fs::write(dir.path().join("hello.txt"), "hello").unwrap();

        let snap1 = ws.snapshot().unwrap();
        assert_eq!(snap1.files.len(), 1);

        // Modify and add a file
        fs::write(dir.path().join("hello.txt"), "world").unwrap();
        fs::write(dir.path().join("new.txt"), "new file").unwrap();

        let snap2 = ws.snapshot().unwrap();

        let diff = Workspace::diff(&snap1, &snap2);
        assert_eq!(diff.created_count(), 1);
        assert_eq!(diff.modified_count(), 1);
        assert_eq!(diff.deleted_count(), 0);
    }

    #[test]
    fn test_diff_deleted_file() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path().to_path_buf());

        fs::write(dir.path().join("gone.txt"), "bye").unwrap();
        let snap1 = ws.snapshot().unwrap();

        fs::remove_file(dir.path().join("gone.txt")).unwrap();
        let snap2 = ws.snapshot().unwrap();

        let diff = Workspace::diff(&snap1, &snap2);
        assert_eq!(diff.deleted_count(), 1);
        assert_eq!(diff.total_count(), 1);
    }

    #[test]
    fn test_hidden_files_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path().to_path_buf());

        fs::write(dir.path().join("visible.txt"), "ok").unwrap();
        fs::create_dir(dir.path().join(".hidden")).unwrap();
        fs::write(dir.path().join(".hidden/secret.txt"), "hidden").unwrap();
        fs::write(dir.path().join(".gitignore"), "hidden").unwrap();

        let snap = ws.snapshot().unwrap();
        assert_eq!(snap.files.len(), 1);
        assert!(snap.files.contains_key(Path::new("visible.txt")));
    }
}
