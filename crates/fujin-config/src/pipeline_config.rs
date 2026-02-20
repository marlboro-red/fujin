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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let yaml = r#"
name: my-pipeline
stages:
  - id: s1
    name: Stage 1
    system_prompt: "Hello"
    user_prompt: "World"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.name, "my-pipeline");
        assert_eq!(config.stages.len(), 1);
        assert_eq!(config.stages[0].id, "s1");
    }

    #[test]
    fn test_defaults_applied() {
        let yaml = r#"
name: defaults-test
stages:
  - id: s1
    name: Stage 1
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.runtime, "claude-code");
        assert_eq!(config.version, "1.0");
        assert_eq!(config.stages[0].model, "claude-sonnet-4-6");
        // Default allowed tools: read, write
        assert_eq!(config.stages[0].allowed_tools, vec!["read", "write"]);
    }

    #[test]
    fn test_per_stage_runtime_override() {
        let yaml = r#"
name: runtime-override
stages:
  - id: s1
    name: Stage 1
    runtime: copilot-cli
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.stages[0].runtime, Some("copilot-cli".to_string()));
    }

    #[test]
    fn test_command_stage_parsing() {
        let yaml = r#"
name: command-pipeline
stages:
  - id: setup
    name: Setup
    commands:
      - "cargo build"
      - "cargo test"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert!(config.stages[0].is_command_stage());
        let cmds = config.stages[0].commands.as_ref().unwrap();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], "cargo build");
    }

    #[test]
    fn test_is_command_stage_false_for_agent() {
        let yaml = r#"
name: agent-pipeline
stages:
  - id: s1
    name: Stage 1
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert!(!config.stages[0].is_command_stage());
    }

    #[test]
    fn test_is_command_stage_false_for_empty_commands() {
        let yaml = r#"
name: empty-cmds
stages:
  - id: s1
    name: Stage 1
    commands: []
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert!(!config.stages[0].is_command_stage());
    }

    #[test]
    fn test_variables_parsed() {
        let yaml = r#"
name: vars-test
variables:
  project_name: my-app
  language: rust
stages:
  - id: s1
    name: Stage 1
    system_prompt: "sp"
    user_prompt: "Build {{project_name}}"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.variables.get("project_name").unwrap(), "my-app");
        assert_eq!(config.variables.get("language").unwrap(), "rust");
    }

    #[test]
    fn test_apply_overrides() {
        let yaml = r#"
name: override-test
variables:
  env: dev
stages:
  - id: s1
    name: Stage 1
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let mut config = PipelineConfig::from_yaml(yaml).unwrap();
        let mut overrides = HashMap::new();
        overrides.insert("env".to_string(), "prod".to_string());
        overrides.insert("new_var".to_string(), "value".to_string());

        config.apply_overrides(&overrides);
        assert_eq!(config.variables.get("env").unwrap(), "prod");
        assert_eq!(config.variables.get("new_var").unwrap(), "value");
    }

    #[test]
    fn test_retry_group_config() {
        let yaml = r#"
name: retry-test
retry_groups:
  fix:
    max_retries: 3
    verify:
      system_prompt: "Verify"
      user_prompt: "Check"
stages:
  - id: s1
    name: Stage 1
    system_prompt: "sp"
    user_prompt: "up"
    retry_group: fix
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        let group = config.retry_groups.get("fix").unwrap();
        assert_eq!(group.max_retries, 3);
        assert!(group.verify.is_some());
        let verify = group.verify.as_ref().unwrap();
        assert_eq!(verify.system_prompt, "Verify");
        assert_eq!(verify.user_prompt, "Check");
    }

    #[test]
    fn test_summarizer_defaults() {
        let yaml = r#"
name: sum-test
stages:
  - id: s1
    name: S1
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.summarizer.model, "claude-haiku-4-5-20251001");
        assert_eq!(config.summarizer.max_tokens, 1024);
    }

    #[test]
    fn test_custom_summarizer() {
        let yaml = r#"
name: sum-test
summarizer:
  model: claude-sonnet-4
  max_tokens: 512
stages:
  - id: s1
    name: S1
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.summarizer.model, "claude-sonnet-4");
        assert_eq!(config.summarizer.max_tokens, 512);
    }

    #[test]
    fn test_stage_timeout() {
        let yaml = r#"
name: timeout-test
stages:
  - id: s1
    name: S1
    system_prompt: "sp"
    user_prompt: "up"
    timeout_secs: 300
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.stages[0].timeout_secs, Some(300));
    }

    #[test]
    fn test_from_file_nonexistent() {
        let result = PipelineConfig::from_file(std::path::Path::new("/nonexistent/file.yaml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_yaml() {
        let result = PipelineConfig::from_yaml("{{{{invalid yaml!!");
        assert!(result.is_err());
    }
}
