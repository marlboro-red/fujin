use crate::PipelineConfig;
use std::collections::HashSet;

/// Validation result containing all issues found.
#[derive(Debug, Default)]
pub struct ValidationResult {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl ValidationResult {
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validate a pipeline configuration.
pub fn validate(config: &PipelineConfig) -> ValidationResult {
    let mut result = ValidationResult::default();

    // Must have a name
    if config.name.trim().is_empty() {
        result.errors.push("Pipeline name must not be empty".to_string());
    }

    // Must have at least one stage
    if config.stages.is_empty() {
        result.errors.push("Pipeline must have at least one stage".to_string());
    }

    // Stage IDs must be unique
    let mut seen_ids = HashSet::new();
    for stage in &config.stages {
        if !seen_ids.insert(&stage.id) {
            result
                .errors
                .push(format!("Duplicate stage id: '{}'", stage.id));
        }
    }

    // Validate each stage
    for (i, stage) in config.stages.iter().enumerate() {
        let prefix = format!("Stage {} ('{}')", i, stage.id);

        if stage.id.trim().is_empty() {
            result.errors.push(format!("{prefix}: id must not be empty"));
        }

        if stage.name.trim().is_empty() {
            result
                .errors
                .push(format!("{prefix}: name must not be empty"));
        }

        if stage.system_prompt.trim().is_empty() {
            result
                .errors
                .push(format!("{prefix}: system_prompt must not be empty"));
        }

        if stage.user_prompt.trim().is_empty() {
            result
                .errors
                .push(format!("{prefix}: user_prompt must not be empty"));
        }

        if stage.max_turns == 0 {
            result
                .errors
                .push(format!("{prefix}: max_turns must be greater than 0"));
        }

        if stage.timeout_secs == 0 {
            result
                .errors
                .push(format!("{prefix}: timeout_secs must be greater than 0"));
        }

        // Validate allowed tools
        let valid_tools = ["read", "write", "bash", "edit", "glob", "grep", "notebook"];
        for tool in &stage.allowed_tools {
            if !valid_tools.contains(&tool.as_str()) {
                result.warnings.push(format!(
                    "{prefix}: unknown tool '{tool}' (valid: {})",
                    valid_tools.join(", ")
                ));
            }
        }
    }

    // Check for {{prior_summary}} usage in first stage
    if let Some(first) = config.stages.first() {
        if first.user_prompt.contains("{{prior_summary}}") {
            result.warnings.push(
                "First stage references {{prior_summary}} which will be empty".to_string(),
            );
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_config() -> PipelineConfig {
        PipelineConfig::from_yaml(
            r#"
name: "Test Pipeline"
stages:
  - id: "stage1"
    name: "Stage One"
    system_prompt: "You are helpful."
    user_prompt: "Do something."
"#,
        )
        .unwrap()
    }

    #[test]
    fn test_valid_minimal_config() {
        let config = minimal_config();
        let result = validate(&config);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_empty_stages() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Empty"
stages: []
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("at least one stage")));
    }

    #[test]
    fn test_duplicate_stage_ids() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Dupes"
stages:
  - id: "same"
    name: "First"
    system_prompt: "x"
    user_prompt: "y"
  - id: "same"
    name: "Second"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("Duplicate stage id")));
    }
}
