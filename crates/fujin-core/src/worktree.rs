use crate::artifact::ArtifactSet;
use crate::error::{CoreError, CoreResult};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::warn;

/// Information about a git worktree created for an isolated stage execution.
#[derive(Debug)]
pub struct WorktreeInfo {
    /// Path to the worktree directory.
    pub path: PathBuf,
    /// Stage ID this worktree was created for.
    pub stage_id: String,
}

/// Check if a path is inside a git repository.
pub fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Record current HEAD commit hash.
pub fn get_head(repo_root: &Path) -> CoreResult<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| CoreError::WorkspaceError {
            path: repo_root.to_path_buf(),
            message: format!("Failed to get HEAD: {e}"),
        })?;

    if !output.status.success() {
        return Err(CoreError::WorkspaceError {
            path: repo_root.to_path_buf(),
            message: format!(
                "git rev-parse HEAD failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Auto-commit all changes in the workspace.
/// Message: "[fujin] stage: <label>"
/// Returns true if a commit was created, false if nothing to commit.
pub fn auto_commit(repo_root: &Path, label: &str) -> CoreResult<bool> {
    // Stage all changes
    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| CoreError::WorkspaceError {
            path: repo_root.to_path_buf(),
            message: format!("git add -A failed: {e}"),
        })?;

    if !add.status.success() {
        return Err(CoreError::WorkspaceError {
            path: repo_root.to_path_buf(),
            message: format!(
                "git add -A failed: {}",
                String::from_utf8_lossy(&add.stderr)
            ),
        });
    }

    // Check if there's anything to commit
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| CoreError::WorkspaceError {
            path: repo_root.to_path_buf(),
            message: format!("git status failed: {e}"),
        })?;

    let stdout = String::from_utf8_lossy(&status.stdout);
    if stdout.trim().is_empty() {
        return Ok(false);
    }

    // Commit
    let msg = format!("[fujin] stage: {label}");
    let commit = Command::new("git")
        .args(["commit", "-m", &msg, "--no-verify"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| CoreError::WorkspaceError {
            path: repo_root.to_path_buf(),
            message: format!("git commit failed: {e}"),
        })?;

    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        // "nothing to commit" is not an error
        if stderr.contains("nothing to commit") {
            return Ok(false);
        }
        return Err(CoreError::WorkspaceError {
            path: repo_root.to_path_buf(),
            message: format!("git commit failed: {stderr}"),
        });
    }

    Ok(true)
}

/// git reset --soft <commit> to undo auto-commits.
pub fn soft_reset(repo_root: &Path, commit: &str) -> CoreResult<()> {
    let output = Command::new("git")
        .args(["reset", "--soft", commit])
        .current_dir(repo_root)
        .output()
        .map_err(|e| CoreError::WorkspaceError {
            path: repo_root.to_path_buf(),
            message: format!("git reset --soft failed: {e}"),
        })?;

    if !output.status.success() {
        return Err(CoreError::WorkspaceError {
            path: repo_root.to_path_buf(),
            message: format!(
                "git reset --soft failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }

    Ok(())
}

/// Worktree base directory for a run.
fn worktree_base(run_id: &str) -> PathBuf {
    std::env::temp_dir()
        .join("fujin-worktrees")
        .join(run_id)
}

/// Create a detached worktree at a temp path from current HEAD.
///
/// Copies uncommitted changes from main workspace into the worktree,
/// then commits a baseline inside the worktree (detached HEAD, doesn't affect main).
pub fn create(repo_root: &Path, run_id: &str, stage_id: &str) -> CoreResult<WorktreeInfo> {
    let wt_path = worktree_base(run_id).join(stage_id);

    // Clean up any leftover worktree at this path
    if wt_path.exists() {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&wt_path)
            .current_dir(repo_root)
            .output();
        let _ = std::fs::remove_dir_all(&wt_path);
    }

    // Create parent dir
    if let Some(parent) = wt_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::WorkspaceError {
            path: parent.to_path_buf(),
            message: format!("Failed to create worktree directory: {e}"),
        })?;
    }

    // git worktree add --detach <path>
    let add = Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&wt_path)
        .current_dir(repo_root)
        .output()
        .map_err(|e| CoreError::WorkspaceError {
            path: repo_root.to_path_buf(),
            message: format!("git worktree add failed: {e}"),
        })?;

    if !add.status.success() {
        return Err(CoreError::WorkspaceError {
            path: repo_root.to_path_buf(),
            message: format!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&add.stderr)
            ),
        });
    }

    // Get list of uncommitted changes from main via `git status --porcelain`
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| CoreError::WorkspaceError {
            path: repo_root.to_path_buf(),
            message: format!("git status failed: {e}"),
        })?;

    let stdout = String::from_utf8_lossy(&status.stdout);
    let mut has_changes = false;

    for line in stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        let status_code = &line[..2];
        let file_path = line[3..].trim();

        // Skip deleted files (nothing to copy)
        if status_code.trim() == "D" {
            // Also delete from worktree if present
            let wt_file = wt_path.join(file_path);
            if wt_file.exists() {
                let _ = std::fs::remove_file(&wt_file);
                has_changes = true;
            }
            continue;
        }

        // Copy the file from repo_root to worktree
        let src = repo_root.join(file_path);
        let dst = wt_path.join(file_path);

        if src.exists() {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            if let Err(e) = std::fs::copy(&src, &dst) {
                warn!(
                    src = %src.display(),
                    dst = %dst.display(),
                    "Failed to copy uncommitted file to worktree: {e}"
                );
            } else {
                has_changes = true;
            }
        }
    }

    // Inside the worktree: git add -A && git commit -m "[fujin] baseline"
    // so that git_diff() later only shows the agent's changes
    if has_changes {
        let _ = Command::new("git")
            .args(["add", "-A"])
            .current_dir(&wt_path)
            .output();

        let _ = Command::new("git")
            .args(["commit", "-m", "[fujin] baseline", "--no-verify", "--allow-empty"])
            .current_dir(&wt_path)
            .output();
    }

    Ok(WorktreeInfo {
        path: wt_path,
        stage_id: stage_id.to_string(),
    })
}

/// Apply file changes from a worktree back to the main workspace.
///
/// For each created/modified file: copy from worktree to main.
/// For each deleted file: remove from main.
/// Returns list of conflicting file paths (modified by multiple parallel stages).
pub fn apply_changes(
    repo_root: &Path,
    worktree: &WorktreeInfo,
    artifacts: &ArtifactSet,
) -> CoreResult<Vec<PathBuf>> {
    let conflicts = Vec::new();

    for change in &artifacts.changes {
        let src = worktree.path.join(&change.path);
        let dst = repo_root.join(&change.path);

        match change.kind {
            crate::artifact::FileChangeKind::Created
            | crate::artifact::FileChangeKind::Modified => {
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| CoreError::WorkspaceError {
                        path: parent.to_path_buf(),
                        message: format!("Failed to create directory: {e}"),
                    })?;
                }
                std::fs::copy(&src, &dst).map_err(|e| CoreError::WorkspaceError {
                    path: dst.clone(),
                    message: format!("Failed to copy from worktree: {e}"),
                })?;
            }
            crate::artifact::FileChangeKind::Deleted => {
                if dst.exists() {
                    std::fs::remove_file(&dst).map_err(|e| CoreError::WorkspaceError {
                        path: dst.clone(),
                        message: format!("Failed to delete file: {e}"),
                    })?;
                }
            }
        }
    }

    Ok(conflicts)
}

/// Remove a worktree and clean up.
pub fn remove(repo_root: &Path, worktree: &WorktreeInfo) -> CoreResult<()> {
    let output = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&worktree.path)
        .current_dir(repo_root)
        .output();

    match output {
        Ok(o) if !o.status.success() => {
            // Force remove the directory if git worktree remove fails
            warn!(
                path = %worktree.path.display(),
                "git worktree remove failed, removing directory directly"
            );
            let _ = std::fs::remove_dir_all(&worktree.path);
        }
        Err(e) => {
            warn!(
                path = %worktree.path.display(),
                "git worktree remove failed: {e}, removing directory directly"
            );
            let _ = std::fs::remove_dir_all(&worktree.path);
        }
        _ => {}
    }

    // Prune stale worktree entries
    let _ = Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(repo_root)
        .output();

    Ok(())
}

/// Remove all worktrees for a given run.
pub fn cleanup_run(run_id: &str) -> CoreResult<()> {
    let base = worktree_base(run_id);
    if base.exists() {
        std::fs::remove_dir_all(&base).map_err(|e| CoreError::WorkspaceError {
            path: base,
            message: format!("Failed to clean up worktree run directory: {e}"),
        })?;
    }
    Ok(())
}
