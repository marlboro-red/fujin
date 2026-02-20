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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_change(path: &str, kind: FileChangeKind) -> FileChange {
        FileChange {
            path: PathBuf::from(path),
            kind,
            hash: None,
            size: None,
        }
    }

    #[test]
    fn test_empty_artifact_set() {
        let set = ArtifactSet::new();
        assert_eq!(set.created_count(), 0);
        assert_eq!(set.modified_count(), 0);
        assert_eq!(set.deleted_count(), 0);
        assert_eq!(set.total_count(), 0);
        assert!(set.changed_paths().is_empty());
        assert_eq!(set.summary(), "no changes");
    }

    #[test]
    fn test_counts_and_summary() {
        let set = ArtifactSet {
            changes: vec![
                make_change("new.txt", FileChangeKind::Created),
                make_change("old.txt", FileChangeKind::Modified),
                make_change("gone.txt", FileChangeKind::Deleted),
                make_change("another.txt", FileChangeKind::Created),
            ],
        };
        assert_eq!(set.created_count(), 2);
        assert_eq!(set.modified_count(), 1);
        assert_eq!(set.deleted_count(), 1);
        assert_eq!(set.total_count(), 4);
        assert_eq!(set.summary(), "2 created, 1 modified, 1 deleted");
    }

    #[test]
    fn test_changed_paths_excludes_deleted() {
        let set = ArtifactSet {
            changes: vec![
                make_change("keep.txt", FileChangeKind::Created),
                make_change("gone.txt", FileChangeKind::Deleted),
                make_change("edit.txt", FileChangeKind::Modified),
            ],
        };
        let paths: Vec<&str> = set
            .changed_paths()
            .iter()
            .map(|p| p.to_str().unwrap())
            .collect();
        assert_eq!(paths, vec!["keep.txt", "edit.txt"]);
    }

    #[test]
    fn test_summary_single_type() {
        let set = ArtifactSet {
            changes: vec![make_change("a.txt", FileChangeKind::Modified)],
        };
        assert_eq!(set.summary(), "1 modified");
    }

    #[test]
    fn test_file_change_kind_display() {
        assert_eq!(format!("{}", FileChangeKind::Created), "created");
        assert_eq!(format!("{}", FileChangeKind::Modified), "modified");
        assert_eq!(format!("{}", FileChangeKind::Deleted), "deleted");
    }
}
