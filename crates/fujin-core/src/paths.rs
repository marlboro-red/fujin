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
