use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The type of change detected for a file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileChangeKind {
    Created,
    Modified,
    Deleted,
}

impl std::fmt::Display for FileChangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "created"),
            Self::Modified => write!(f, "modified"),
            Self::Deleted => write!(f, "deleted"),
        }
    }
}

/// A single file change detected between workspace snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    /// Relative path within the workspace.
    pub path: PathBuf,

    /// What kind of change occurred.
    pub kind: FileChangeKind,

    /// SHA-256 hash of the file after the change (None if deleted).
    pub hash: Option<String>,

    /// File size in bytes after the change (None if deleted).
    pub size: Option<u64>,
}

/// A set of file changes produced by a single stage.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtifactSet {
    pub changes: Vec<FileChange>,
}

impl ArtifactSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of files created.
    pub fn created_count(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| c.kind == FileChangeKind::Created)
            .count()
    }

    /// Number of files modified.
    pub fn modified_count(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| c.kind == FileChangeKind::Modified)
            .count()
    }

    /// Number of files deleted.
    pub fn deleted_count(&self) -> usize {
        self.changes
            .iter()
            .filter(|c| c.kind == FileChangeKind::Deleted)
            .count()
    }

    /// Total number of changes.
    pub fn total_count(&self) -> usize {
        self.changes.len()
    }

    /// List of all file paths that were created or modified.
    pub fn changed_paths(&self) -> Vec<&PathBuf> {
        self.changes
            .iter()
            .filter(|c| c.kind != FileChangeKind::Deleted)
            .map(|c| &c.path)
            .collect()
    }

    /// Human-readable summary of changes.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        let created = self.created_count();
        let modified = self.modified_count();
        let deleted = self.deleted_count();

        if created > 0 {
            parts.push(format!("{created} created"));
        }
        if modified > 0 {
            parts.push(format!("{modified} modified"));
        }
        if deleted > 0 {
            parts.push(format!("{deleted} deleted"));
        }

        if parts.is_empty() {
            "no changes".to_string()
        } else {
            parts.join(", ")
        }
    }
}
