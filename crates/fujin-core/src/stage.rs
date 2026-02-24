use crate::artifact::ArtifactSet;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Result of executing a single stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageResult {
    /// Stage ID from config.
    pub stage_id: String,

    /// Model used for this stage (empty for command stages).
    #[serde(default)]
    pub model: String,

    /// The agent's text response.
    pub response_text: String,

    /// Files created/modified/deleted during this stage.
    pub artifacts: ArtifactSet,

    /// Summary of what this stage accomplished (generated between stages).
    #[serde(default)]
    pub summary: Option<String>,

    /// How long the stage took.
    pub duration: Duration,

    /// When the stage completed.
    pub completed_at: DateTime<Utc>,

    /// Token usage if available.
    #[serde(default)]
    pub token_usage: Option<TokenUsage>,
}

/// Token usage statistics from the agent runtime.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Premium requests consumed (Copilot CLI only).
    #[serde(default)]
    pub premium_requests: Option<u32>,
}
