use crate::error::{CoreError, CoreResult};
use crate::stage::StageResult;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tracing::{debug, info};
use uuid::Uuid;

/// A checkpoint captures the state of a pipeline run after each stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Unique run identifier.
    pub run_id: String,

    /// SHA-256 hash of the pipeline config (to detect config changes).
    pub config_hash: String,

    /// Index of the next stage to execute (0-based).
    pub next_stage_index: usize,

    /// Results from all completed stages.
    pub completed_stages: Vec<StageResult>,

    /// When this checkpoint was created.
    pub created_at: DateTime<Utc>,

    /// When this checkpoint was last updated.
    pub updated_at: DateTime<Utc>,
}

/// Manages checkpoint persistence in the `.fujin/checkpoints/` directory.
pub struct CheckpointManager {
    checkpoint_dir: PathBuf,
}

impl CheckpointManager {
    /// Create a new checkpoint manager. The checkpoint directory is
    /// `<workspace>/.fujin/checkpoints/`.
    pub fn new(workspace_root: &Path) -> Self {
        Self {
            checkpoint_dir: workspace_root.join(".fujin").join("checkpoints"),
        }
    }

    /// Ensure the checkpoint directory exists.
    pub fn ensure_dir(&self) -> CoreResult<()> {
        std::fs::create_dir_all(&self.checkpoint_dir)?;
        Ok(())
    }

    /// Create a new checkpoint for a fresh run.
    pub fn create_new(config_yaml: &str) -> Checkpoint {
        let now = Utc::now();
        Checkpoint {
            run_id: Uuid::new_v4().to_string(),
            config_hash: hash_config(config_yaml),
            next_stage_index: 0,
            completed_stages: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Save a checkpoint to disk.
    pub fn save(&self, checkpoint: &Checkpoint) -> CoreResult<()> {
        self.ensure_dir()?;

        let json = serde_json::to_string_pretty(checkpoint)?;

        let path = self.checkpoint_path(&checkpoint.run_id);
        std::fs::write(&path, &json)?;

        // Also save as "latest" copy for easy resume
        let latest_path = self.checkpoint_dir.join("latest.json");
        std::fs::write(&latest_path, &json)?;

        debug!(
            run_id = %checkpoint.run_id,
            path = %path.display(),
            "Saved checkpoint"
        );
        Ok(())
    }

    /// Load the latest checkpoint for resume.
    pub fn load_latest(&self) -> CoreResult<Option<Checkpoint>> {
        let latest_path = self.checkpoint_dir.join("latest.json");

        if !latest_path.exists() {
            return Ok(None);
        }

        let json = std::fs::read_to_string(&latest_path).map_err(|e| {
            CoreError::CheckpointError {
                message: format!("Failed to read latest checkpoint: {e}"),
            }
        })?;

        let checkpoint: Checkpoint =
            serde_json::from_str(&json).map_err(|e| CoreError::CheckpointError {
                message: format!("Failed to parse checkpoint JSON: {e}"),
            })?;

        info!(
            run_id = %checkpoint.run_id,
            next_stage = checkpoint.next_stage_index,
            "Loaded checkpoint"
        );
        Ok(Some(checkpoint))
    }

    /// Load a specific checkpoint by run ID.
    pub fn load(&self, run_id: &str) -> CoreResult<Option<Checkpoint>> {
        let path = self.checkpoint_path(run_id);

        if !path.exists() {
            return Ok(None);
        }

        let json = std::fs::read_to_string(&path).map_err(|e| {
            CoreError::CheckpointError {
                message: format!("Failed to read checkpoint {run_id}: {e}"),
            }
        })?;

        let checkpoint: Checkpoint =
            serde_json::from_str(&json).map_err(|e| CoreError::CheckpointError {
                message: format!("Failed to parse checkpoint JSON: {e}"),
            })?;

        Ok(Some(checkpoint))
    }

    /// List all checkpoint run IDs.
    pub fn list(&self) -> CoreResult<Vec<CheckpointSummary>> {
        let mut summaries = Vec::new();

        if !self.checkpoint_dir.exists() {
            return Ok(summaries);
        }

        for entry in std::fs::read_dir(&self.checkpoint_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|e| e == "json")
                && path.file_stem().is_some_and(|s| s != "latest")
            {
                if let Ok(json) = std::fs::read_to_string(&path) {
                    if let Ok(cp) = serde_json::from_str::<Checkpoint>(&json) {
                        summaries.push(CheckpointSummary {
                            run_id: cp.run_id,
                            next_stage_index: cp.next_stage_index,
                            completed_stages: cp.completed_stages.len(),
                            created_at: cp.created_at,
                            updated_at: cp.updated_at,
                        });
                    }
                }
            }
        }

        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(summaries)
    }

    /// Remove all checkpoints.
    pub fn clean(&self) -> CoreResult<usize> {
        if !self.checkpoint_dir.exists() {
            return Ok(0);
        }

        let mut count = 0;
        for entry in std::fs::read_dir(&self.checkpoint_dir)? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|e| e == "json") {
                std::fs::remove_file(entry.path())?;
                count += 1;
            }
        }

        info!(count, "Cleaned checkpoints");
        Ok(count)
    }

    /// Validate that a checkpoint is compatible with the current config.
    pub fn validate_resume(
        checkpoint: &Checkpoint,
        config_yaml: &str,
    ) -> CoreResult<()> {
        let current_hash = hash_config(config_yaml);
        if checkpoint.config_hash != current_hash {
            return Err(CoreError::CheckpointError {
                message: "Pipeline config has changed since the checkpoint was created. \
                         Use --no-resume or clean checkpoints to start fresh."
                    .to_string(),
            });
        }
        Ok(())
    }

    fn checkpoint_path(&self, run_id: &str) -> PathBuf {
        self.checkpoint_dir.join(format!("{run_id}.json"))
    }
}

/// Summary of a checkpoint for listing.
#[derive(Debug, Serialize)]
pub struct CheckpointSummary {
    pub run_id: String,
    pub next_stage_index: usize,
    pub completed_stages: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Hash a config string for change detection.
fn hash_config(config_yaml: &str) -> String {
    format!("{:x}", Sha256::digest(config_yaml.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_new_checkpoint() {
        let cp = CheckpointManager::create_new("name: test\nstages: []");
        assert_eq!(cp.next_stage_index, 0);
        assert!(cp.completed_stages.is_empty());
        assert!(!cp.run_id.is_empty());
    }

    #[test]
    fn test_config_hash_consistency() {
        let yaml = "name: test\nstages: []";
        let h1 = hash_config(yaml);
        let h2 = hash_config(yaml);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_config_hash_changes() {
        let h1 = hash_config("name: a");
        let h2 = hash_config("name: b");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_save_and_load_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let manager = CheckpointManager::new(dir.path());

        let cp = CheckpointManager::create_new("test config");
        manager.save(&cp).unwrap();

        let loaded = manager.load_latest().unwrap().unwrap();
        assert_eq!(loaded.run_id, cp.run_id);
        assert_eq!(loaded.config_hash, cp.config_hash);
    }

    #[test]
    fn test_list_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let manager = CheckpointManager::new(dir.path());

        let cp1 = CheckpointManager::create_new("config1");
        manager.save(&cp1).unwrap();

        let cp2 = CheckpointManager::create_new("config2");
        manager.save(&cp2).unwrap();

        let list = manager.list().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_clean_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let manager = CheckpointManager::new(dir.path());

        let cp = CheckpointManager::create_new("config");
        manager.save(&cp).unwrap();

        let count = manager.clean().unwrap();
        assert!(count > 0);

        let list = manager.list().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn test_validate_resume_matching_config() {
        let yaml = "name: test";
        let cp = CheckpointManager::create_new(yaml);
        assert!(CheckpointManager::validate_resume(&cp, yaml).is_ok());
    }

    #[test]
    fn test_validate_resume_changed_config() {
        let cp = CheckpointManager::create_new("name: original");
        let result = CheckpointManager::validate_resume(&cp, "name: changed");
        assert!(result.is_err());
    }
}
