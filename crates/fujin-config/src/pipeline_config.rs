use serde::{Deserialize, Deserializer, Serialize};
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

    /// Included pipelines whose stages are merged into this pipeline.
    #[serde(default)]
    pub includes: Vec<IncludeConfig>,

    /// Enable git-worktree isolation for parallel agent stages.
    /// When `true`, each parallel agent stage runs in its own worktree so
    /// concurrent file edits don't conflict.  Defaults to `false`.
    #[serde(default)]
    pub worktrees: bool,

    /// Ordered list of pipeline stages.
    pub stages: Vec<StageConfig>,
}

/// Configuration for including stages from another pipeline file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncludeConfig {
    /// Path to the pipeline YAML file (resolved relative to the parent file).
    pub source: String,

    /// Prefix alias for all stage IDs from this include.
    /// Stage "build" with as: "be" becomes "be.build".
    #[serde(rename = "as")]
    pub alias: String,

    /// Stages in the parent DAG that the included pipeline's root stages depend on.
    #[serde(default, alias = "depends_on")]
    pub dependencies: Vec<String>,

    /// Variables to pass into the included pipeline.
    /// These override the included pipeline's own variable defaults.
    #[serde(default)]
    pub vars: HashMap<String, String>,
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

/// Configuration for exporting agent-set variables from a stage.
///
/// After a stage completes, the pipeline runner reads a JSON file from the
/// platform data directory and merges its key-value pairs into the pipeline's
/// template variables. The export file path is auto-computed and injected as
/// `{{exports_file}}` — the agent writes to that path, and the runner reads it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportsConfig {
    /// Optional list of expected keys. If specified, the runner emits a warning
    /// for any keys that are missing from the file.
    #[serde(default)]
    pub keys: Vec<String>,
}

/// Condition for gating stage execution based on a prior stage's output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhenCondition {
    /// Stage ID whose `response_text` to check.
    pub stage: String,
    /// Regex pattern (case-insensitive) to match against the stage's output.
    pub output_matches: String,
}

/// AI-driven branch classifier that runs after a stage completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchConfig {
    /// Model for the classifier (defaults to summarizer model).
    #[serde(default = "default_summarizer_model")]
    pub model: String,
    /// Prompt sent to the classifier along with the stage's output.
    pub prompt: String,
    /// Valid route names the classifier can select.
    pub routes: Vec<String>,
    /// Fallback route if classifier output doesn't match any route.
    #[serde(default)]
    pub default: Option<String>,
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

    /// Condition for skipping this stage based on a prior stage's output.
    #[serde(default)]
    pub when: Option<WhenCondition>,

    /// AI-driven branch classifier that runs after this stage completes.
    #[serde(default)]
    pub branch: Option<BranchConfig>,

    /// Only run this stage if a matching branch route was selected.
    /// Accepts a single string or a list of strings (OR semantics).
    /// `None` means the stage always runs (no branch gating).
    #[serde(default, deserialize_with = "deserialize_on_branch")]
    pub on_branch: Option<Vec<String>>,

    /// Export agent-set variables from this stage.
    /// After the stage completes, the runner reads the specified JSON file
    /// and merges its key-value pairs into the pipeline's template variables.
    #[serde(default)]
    pub exports: Option<ExportsConfig>,

    /// Explicit stage dependencies. This stage waits for all listed stages to complete.
    /// When absent (`None`), the stage implicitly depends on the previous stage in YAML order.
    /// Use `dependencies: []` to declare a stage with no dependencies (can start immediately).
    #[serde(default, alias = "depends_on")]
    pub dependencies: Option<Vec<String>>,
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

/// Deserialize `on_branch` from either a single string or a list of strings.
fn deserialize_on_branch<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        Single(String),
        Multiple(Vec<String>),
    }

    let value: Option<StringOrVec> = Option::deserialize(deserializer)?;
    Ok(value.map(|v| match v {
        StringOrVec::Single(s) => vec![s],
        StringOrVec::Multiple(v) => v,
    }))
}

impl PipelineConfig {
    /// Parse a pipeline config from a YAML string.
    pub fn from_yaml(yaml: &str) -> Result<Self, crate::ConfigError> {
        let config: Self = serde_yml::from_str(yaml)?;
        Ok(config)
    }

    /// Parse a pipeline config from a YAML string and validate it.
    ///
    /// Returns the parsed config along with any validation warnings.
    /// Fails if validation produces errors.
    pub fn from_yaml_validated(yaml: &str) -> Result<(Self, Vec<String>), crate::ConfigError> {
        let config = Self::from_yaml(yaml)?;
        let result = crate::validate(&config);
        if !result.is_valid() {
            return Err(crate::ConfigError::Validation {
                errors: result.errors,
                warnings: result.warnings,
            });
        }
        Ok((config, result.warnings))
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

    /// Load a pipeline config from a YAML file and validate it.
    ///
    /// Returns the parsed config along with any validation warnings.
    /// Fails if validation produces errors.
    pub fn from_file_validated(path: &std::path::Path) -> Result<(Self, Vec<String>), crate::ConfigError> {
        let config = Self::from_file(path)?;
        let result = crate::validate(&config);
        if !result.is_valid() {
            return Err(crate::ConfigError::Validation {
                errors: result.errors,
                warnings: result.warnings,
            });
        }
        Ok((config, result.warnings))
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

    #[test]
    fn test_when_condition_parsing() {
        let yaml = r#"
name: when-test
stages:
  - id: analyze
    name: Analyze
    system_prompt: "sp"
    user_prompt: "up"
  - id: frontend
    name: Frontend
    when:
      stage: analyze
      output_matches: "FRONTEND|FULLSTACK"
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert!(config.stages[0].when.is_none());
        let when = config.stages[1].when.as_ref().unwrap();
        assert_eq!(when.stage, "analyze");
        assert_eq!(when.output_matches, "FRONTEND|FULLSTACK");
    }

    #[test]
    fn test_branch_config_parsing() {
        let yaml = r#"
name: branch-test
stages:
  - id: analyze
    name: Analyze
    system_prompt: "sp"
    user_prompt: "up"
    branch:
      prompt: "Classify the work"
      routes: [frontend, backend, fullstack]
      default: fullstack
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        let branch = config.stages[0].branch.as_ref().unwrap();
        assert_eq!(branch.prompt, "Classify the work");
        assert_eq!(branch.routes, vec!["frontend", "backend", "fullstack"]);
        assert_eq!(branch.default, Some("fullstack".to_string()));
        assert_eq!(branch.model, "claude-haiku-4-5-20251001");
    }

    #[test]
    fn test_on_branch_single_string() {
        let yaml = r#"
name: on-branch-test
stages:
  - id: s1
    name: S1
    on_branch: frontend
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert_eq!(
            config.stages[0].on_branch,
            Some(vec!["frontend".to_string()])
        );
    }

    #[test]
    fn test_on_branch_list() {
        let yaml = r#"
name: on-branch-list-test
stages:
  - id: s1
    name: S1
    on_branch: [frontend, fullstack]
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert_eq!(
            config.stages[0].on_branch,
            Some(vec!["frontend".to_string(), "fullstack".to_string()])
        );
    }

    #[test]
    fn test_on_branch_absent() {
        let yaml = r#"
name: no-on-branch
stages:
  - id: s1
    name: S1
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert!(config.stages[0].on_branch.is_none());
    }

    #[test]
    fn test_exports_config_with_keys() {
        let yaml = r#"
name: exports-test
stages:
  - id: analyze
    name: Analyze
    system_prompt: "sp"
    user_prompt: "up"
    exports:
      keys: [language, framework]
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        let exports = config.stages[0].exports.as_ref().unwrap();
        assert_eq!(exports.keys, vec!["language", "framework"]);
    }

    #[test]
    fn test_exports_config_empty() {
        let yaml = r#"
name: exports-test
stages:
  - id: analyze
    name: Analyze
    system_prompt: "sp"
    user_prompt: "up"
    exports: {}
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        let exports = config.stages[0].exports.as_ref().unwrap();
        assert!(exports.keys.is_empty());
    }

    #[test]
    fn test_depends_on_parsing() {
        let yaml = r#"
name: dag-test
stages:
  - id: analyze
    name: Analyze
    system_prompt: "sp"
    user_prompt: "up"
  - id: frontend
    name: Frontend
    depends_on: [analyze]
    system_prompt: "sp"
    user_prompt: "up"
  - id: backend
    name: Backend
    depends_on: [analyze]
    system_prompt: "sp"
    user_prompt: "up"
  - id: integrate
    name: Integration
    depends_on: [frontend, backend]
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert!(config.stages[0].dependencies.is_none());
        assert_eq!(
            config.stages[1].dependencies,
            Some(vec!["analyze".to_string()])
        );
        assert_eq!(
            config.stages[2].dependencies,
            Some(vec!["analyze".to_string()])
        );
        assert_eq!(
            config.stages[3].dependencies,
            Some(vec!["frontend".to_string(), "backend".to_string()])
        );
    }

    #[test]
    fn test_depends_on_empty_list() {
        let yaml = r#"
name: dag-test
stages:
  - id: s1
    name: S1
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert_eq!(config.stages[0].dependencies, Some(vec![]));
    }

    #[test]
    fn test_depends_on_absent_by_default() {
        let yaml = r#"
name: no-deps
stages:
  - id: s1
    name: S1
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert!(config.stages[0].dependencies.is_none());
    }

    #[test]
    fn test_exports_absent_by_default() {
        let yaml = r#"
name: no-exports
stages:
  - id: s1
    name: S1
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let config = PipelineConfig::from_yaml(yaml).unwrap();
        assert!(config.stages[0].exports.is_none());
    }

    #[test]
    fn test_from_yaml_validated_valid() {
        let yaml = r#"
name: valid-pipeline
stages:
  - id: s1
    name: S1
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let (config, warnings) = PipelineConfig::from_yaml_validated(yaml).unwrap();
        assert_eq!(config.name, "valid-pipeline");
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_from_yaml_validated_invalid() {
        let yaml = r#"
name: ""
stages: []
"#;
        let err = PipelineConfig::from_yaml_validated(yaml).unwrap_err();
        match err {
            crate::ConfigError::Validation { errors, .. } => {
                assert!(!errors.is_empty());
            }
            _ => panic!("Expected Validation error, got: {err:?}"),
        }
    }

    #[test]
    fn test_from_yaml_validated_with_warnings() {
        let yaml = r#"
name: warn-pipeline
runtime: "unknown-runtime"
stages:
  - id: s1
    name: S1
    system_prompt: "sp"
    user_prompt: "up"
"#;
        let (config, warnings) = PipelineConfig::from_yaml_validated(yaml).unwrap();
        assert_eq!(config.name, "warn-pipeline");
        assert!(!warnings.is_empty());
        assert!(warnings.iter().any(|w| w.contains("Unknown pipeline runtime")));
    }
}
