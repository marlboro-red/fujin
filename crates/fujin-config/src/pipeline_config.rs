use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Top-level pipeline configuration parsed from YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// Human-readable pipeline name.
    pub name: String,

    /// Config version string.
    #[serde(default = "default_version")]
    pub version: String,

    /// Template variables available in prompt rendering.
    #[serde(default)]
    pub variables: HashMap<String, String>,

    /// Summarizer configuration for inter-stage context passing.
    #[serde(default)]
    pub summarizer: SummarizerConfig,

    /// Ordered list of pipeline stages.
    pub stages: Vec<StageConfig>,
}

/// Configuration for the inter-stage summarizer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizerConfig {
    /// Model to use for summarization.
    #[serde(default = "default_summarizer_model")]
    pub model: String,

    /// Maximum tokens for the summary.
    #[serde(default = "default_summarizer_max_tokens")]
    pub max_tokens: u32,
}

impl Default for SummarizerConfig {
    fn default() -> Self {
        Self {
            model: default_summarizer_model(),
            max_tokens: default_summarizer_max_tokens(),
        }
    }
}

/// Configuration for a single pipeline stage.
///
/// A stage is either an **agent stage** (uses Claude Code with prompts) or a
/// **command stage** (runs a series of CLI commands). When `commands` is set,
/// the stage is treated as a command stage and agent-specific fields
/// (`system_prompt`, `user_prompt`, `model`, etc.) are ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageConfig {
    /// Unique identifier for this stage.
    pub id: String,

    /// Human-readable stage name.
    pub name: String,

    /// Model to use for this stage (agent stages only).
    #[serde(default = "default_stage_model")]
    pub model: String,

    /// System prompt for the agent (agent stages only).
    #[serde(default)]
    pub system_prompt: String,

    /// User prompt template (supports {{variables}}) (agent stages only).
    #[serde(default)]
    pub user_prompt: String,

    /// Maximum agentic turns for the Claude Code agent (agent stages only).
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,

    /// Optional stage timeout in seconds. If `None`, stages run without a time limit.
    #[serde(default)]
    pub timeout_secs: Option<u64>,

    /// Which tools Claude Code can use in this stage (agent stages only).
    #[serde(default = "default_allowed_tools")]
    pub allowed_tools: Vec<String>,

    /// CLI commands to run sequentially (command stages only).
    /// When set, this stage runs these commands instead of an agent.
    /// Each command is executed as a shell command in the workspace directory.
    /// Commands support `{{variable}}` template syntax.
    #[serde(default)]
    pub commands: Option<Vec<String>>,
}

impl StageConfig {
    /// Returns `true` if this is a command stage (runs CLI commands, not an agent).
    pub fn is_command_stage(&self) -> bool {
        self.commands.as_ref().is_some_and(|c| !c.is_empty())
    }
}

fn default_version() -> String {
    "1.0".to_string()
}

fn default_summarizer_model() -> String {
    "claude-haiku-4-5-20251001".to_string()
}

fn default_summarizer_max_tokens() -> u32 {
    1024
}

fn default_stage_model() -> String {
    "claude-sonnet-4-6".to_string()
}

fn default_max_turns() -> u32 {
    10
}

fn default_allowed_tools() -> Vec<String> {
    vec!["read".to_string(), "write".to_string()]
}

impl PipelineConfig {
    /// Parse a pipeline config from a YAML string.
    pub fn from_yaml(yaml: &str) -> Result<Self, crate::ConfigError> {
        let config: Self = serde_yml::from_str(yaml)?;
        Ok(config)
    }

    /// Load a pipeline config from a YAML file.
    pub fn from_file(path: &std::path::Path) -> Result<Self, crate::ConfigError> {
        let contents = std::fs::read_to_string(path).map_err(|e| {
            crate::ConfigError::FileRead {
                path: path.to_path_buf(),
                source: e,
            }
        })?;
        Self::from_yaml(&contents)
    }

    /// Apply variable overrides (from --var CLI flags).
    pub fn apply_overrides(&mut self, overrides: &HashMap<String, String>) {
        for (key, value) in overrides {
            self.variables.insert(key.clone(), value.clone());
        }
    }
}
