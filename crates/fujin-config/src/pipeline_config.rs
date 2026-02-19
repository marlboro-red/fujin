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

    /// Default agent runtime for all stages (e.g., "claude-code", "copilot-cli").
    /// Individual stages can override this with their own `runtime` field.
    #[serde(default = "default_runtime")]
    pub runtime: String,

    /// Template variables available in prompt rendering.
    #[serde(default)]
    pub variables: HashMap<String, String>,

    /// Summarizer configuration for inter-stage context passing.
    #[serde(default)]
    pub summarizer: SummarizerConfig,

    /// Retry group definitions. Stages referencing a group loop as a unit on failure.
    #[serde(default)]
    pub retry_groups: HashMap<String, RetryGroupConfig>,

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

/// Configuration for a retry group.
///
/// Stages sharing a retry group are re-executed as a unit when any stage
/// in the group fails. After `max_retries` consecutive failures the runner
/// prompts the user to continue (granting another `max_retries` attempts).
///
/// An optional `verify` agent can be configured. After the last stage in the
/// group succeeds, the verify agent runs and must output `PASS` or `FAIL` as
/// its verdict. If the verdict is `FAIL`, the group loops back to its first
/// stage (counting toward `max_retries`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryGroupConfig {
    /// Maximum retries before prompting the user to continue.
    #[serde(default = "default_retry_max_retries")]
    pub max_retries: u32,

    /// Optional verification agent that judges whether the group's work is correct.
    #[serde(default)]
    pub verify: Option<VerifyConfig>,
}

impl Default for RetryGroupConfig {
    fn default() -> Self {
        Self {
            max_retries: default_retry_max_retries(),
            verify: None,
        }
    }
}

/// Configuration for a verification agent attached to a retry group.
///
/// The verify agent runs after the last stage in the group succeeds. It
/// receives the configured prompts and must include either `PASS` or `FAIL`
/// in its response to indicate whether the group's work is acceptable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyConfig {
    /// Model to use for verification (defaults to a fast model).
    #[serde(default = "default_verify_model")]
    pub model: String,

    /// System prompt for the verification agent.
    pub system_prompt: String,

    /// User prompt template (supports {{variables}}).
    pub user_prompt: String,

    /// Which tools the verification agent can use.
    #[serde(default = "default_verify_tools")]
    pub allowed_tools: Vec<String>,

    /// Optional timeout in seconds for the verification agent.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Configuration for a single pipeline stage.
///
/// A stage is either an **agent stage** (uses an AI agent with prompts) or a
/// **command stage** (runs a series of CLI commands). When `commands` is set,
/// the stage is treated as a command stage and agent-specific fields
/// (`system_prompt`, `user_prompt`, `model`, etc.) are ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageConfig {
    /// Unique identifier for this stage.
    pub id: String,

    /// Human-readable stage name.
    pub name: String,

    /// Agent runtime override for this specific stage.
    /// When set, this stage uses the specified runtime instead of the pipeline default.
    /// Valid values: "claude-code", "copilot-cli".
    #[serde(default)]
    pub runtime: Option<String>,

    /// Model to use for this stage (agent stages only).
    #[serde(default = "default_stage_model")]
    pub model: String,

    /// System prompt for the agent (agent stages only).
    #[serde(default)]
    pub system_prompt: String,

    /// User prompt template (supports {{variables}}) (agent stages only).
    #[serde(default)]
    pub user_prompt: String,

    /// Optional stage timeout in seconds. If `None`, stages run without a time limit.
    #[serde(default)]
    pub timeout_secs: Option<u64>,

    /// Which tools the agent can use in this stage (agent stages only).
    #[serde(default = "default_allowed_tools")]
    pub allowed_tools: Vec<String>,

    /// CLI commands to run sequentially (command stages only).
    /// When set, this stage runs these commands instead of an agent.
    /// Each command is executed as a shell command in the workspace directory.
    /// Commands support `{{variable}}` template syntax.
    #[serde(default)]
    pub commands: Option<Vec<String>>,

    /// Optional retry group name. Stages sharing the same retry group are
    /// re-executed as a unit when any stage in the group fails.
    #[serde(default)]
    pub retry_group: Option<String>,
}

impl StageConfig {
    /// Returns `true` if this is a command stage (runs CLI commands, not an agent).
    pub fn is_command_stage(&self) -> bool {
        self.commands.as_ref().is_some_and(|c| !c.is_empty())
    }
}

/// Known runtime identifiers.
pub const KNOWN_RUNTIMES: &[&str] = &["claude-code", "copilot-cli"];

fn default_runtime() -> String {
    "claude-code".to_string()
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

fn default_retry_max_retries() -> u32 {
    5
}

fn default_allowed_tools() -> Vec<String> {
    vec!["read".to_string(), "write".to_string()]
}

fn default_verify_model() -> String {
    "claude-haiku-4-5-20251001".to_string()
}

fn default_verify_tools() -> Vec<String> {
    vec!["read".to_string(), "bash".to_string()]
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
