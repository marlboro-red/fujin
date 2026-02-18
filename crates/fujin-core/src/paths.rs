use std::path::PathBuf;

use anyhow::{Context, Result};

const APP_NAME: &str = "fujin";

/// Returns the platform-specific data directory for fujin.
///
/// - macOS: `~/Library/Application Support/fujin/`
/// - Linux: `~/.local/share/fujin/`
/// - Windows: `%LOCALAPPDATA%/fujin/`
///
/// Falls back to `~/.fujin/` if the platform directory cannot be determined.
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
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

/// Creates the data directory structure if it doesn't exist.
pub fn ensure_dirs() -> Result<()> {
    let dirs = [templates_dir(), configs_dir()];
    for dir in &dirs {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create directory: {}", dir.display()))?;
    }
    Ok(())
}
