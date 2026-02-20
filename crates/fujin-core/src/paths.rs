use crate::error::{CoreError, CoreResult};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const APP_NAME: &str = "fujin";

/// Returns the platform-specific data directory for fujin.
///
/// - macOS: `~/Library/Application Support/fujin/`
/// - Linux: `~/.local/share/fujin/`
/// - Windows: `%LOCALAPPDATA%/fujin/`
///
/// Falls back to `~/.fujin/` if the platform directory cannot be determined.
pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(format!(".{APP_NAME}"))
        })
        .join(APP_NAME)
}

/// Returns the templates directory: `<data_dir>/templates/`
pub fn templates_dir() -> PathBuf {
    data_dir().join("templates")
}

/// Returns the configs directory: `<data_dir>/configs/`
pub fn configs_dir() -> PathBuf {
    data_dir().join("configs")
}

/// Returns the checkpoints directory for a specific workspace.
///
/// Checkpoints are stored under `<data_dir>/checkpoints/<hash>/` where
/// `<hash>` is the hex-encoded SHA-256 of the workspace path. This keeps
/// checkpoint data out of the repository.
pub fn checkpoints_dir(workspace_root: &Path) -> PathBuf {
    let canonical = workspace_root
        .to_string_lossy()
        .to_lowercase()
        .replace('\\', "/");
    let hash = format!("{:x}", Sha256::digest(canonical.as_bytes()));
    data_dir().join("checkpoints").join(hash)
}

/// Creates the data directory structure if it doesn't exist.
pub fn ensure_dirs() -> CoreResult<()> {
    let dirs = [templates_dir(), configs_dir()];
    for dir in &dirs {
        std::fs::create_dir_all(dir).map_err(|e| CoreError::WorkspaceError {
            path: dir.clone(),
            message: format!("Failed to create directory: {e}"),
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_dir_ends_with_fujin() {
        let dir = data_dir();
        assert!(
            dir.ends_with("fujin"),
            "data_dir should end with 'fujin', got: {}",
            dir.display()
        );
    }

    #[test]
    fn test_templates_dir_is_under_data_dir() {
        let templates = templates_dir();
        let data = data_dir();
        assert!(templates.starts_with(&data));
        assert!(templates.ends_with("templates"));
    }

    #[test]
    fn test_configs_dir_is_under_data_dir() {
        let configs = configs_dir();
        let data = data_dir();
        assert!(configs.starts_with(&data));
        assert!(configs.ends_with("configs"));
    }

    #[test]
    fn test_checkpoints_dir_uses_hash() {
        let workspace = Path::new("/tmp/test-workspace");
        let cp_dir = checkpoints_dir(workspace);
        assert!(cp_dir.to_string_lossy().contains("checkpoints"));
        // The hash directory name should be a hex string
        let hash_component = cp_dir.file_name().unwrap().to_string_lossy();
        assert!(hash_component.len() == 64, "SHA-256 hash should be 64 hex chars");
        assert!(hash_component.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_checkpoints_dir_deterministic() {
        let workspace = Path::new("/tmp/test-workspace");
        let dir1 = checkpoints_dir(workspace);
        let dir2 = checkpoints_dir(workspace);
        assert_eq!(dir1, dir2, "Same workspace should produce same checkpoint dir");
    }

    #[test]
    fn test_checkpoints_dir_different_workspaces() {
        let dir1 = checkpoints_dir(Path::new("/workspace/a"));
        let dir2 = checkpoints_dir(Path::new("/workspace/b"));
        assert_ne!(dir1, dir2, "Different workspaces should produce different checkpoint dirs");
    }

    #[test]
    fn test_ensure_dirs_creates_directories() {
        // This creates real directories on the filesystem. They already exist
        // in normal use, so this is safe.
        ensure_dirs().expect("ensure_dirs should succeed");
        assert!(templates_dir().exists());
        assert!(configs_dir().exists());
    }
}
