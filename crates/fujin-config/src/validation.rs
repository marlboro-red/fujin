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

        if stage.is_command_stage() {
            // Command stage validation
            let commands = stage.commands.as_ref().unwrap();
            for (ci, cmd) in commands.iter().enumerate() {
                if cmd.trim().is_empty() {
                    result.errors.push(format!(
                        "{prefix}: command[{ci}] must not be empty"
                    ));
                }
            }
        } else {
            // Agent stage validation
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

        if stage.timeout_secs == Some(0) {
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

    // Validate retry groups
    // 1. All retry_group references must exist in retry_groups map
    for (i, stage) in config.stages.iter().enumerate() {
        if let Some(ref group) = stage.retry_group {
            if !config.retry_groups.contains_key(group) {
                result.errors.push(format!(
                    "Stage {} ('{}'): retry_group '{}' is not defined in retry_groups",
                    i, stage.id, group
                ));
            }
        }
    }

    // 2. Stages in the same retry group must be consecutive
    // 3. Each retry group must have at least 2 stages
    let mut group_ranges: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, stage) in config.stages.iter().enumerate() {
        if let Some(ref group) = stage.retry_group {
            group_ranges.entry(group.as_str()).or_default().push(i);
        }
    }

    for (group_name, indices) in &group_ranges {
        if indices.len() < 2 {
            // A single-stage group is valid when the group has a verify agent
            let has_verify = config
                .retry_groups
                .get(*group_name)
                .is_some_and(|g| g.verify.is_some());
            if !has_verify {
                result.errors.push(format!(
                    "Retry group '{}' must contain at least 2 stages (or use a verify agent)",
                    group_name
                ));
            }
        }
        // Check consecutiveness
        for window in indices.windows(2) {
            if window[1] != window[0] + 1 {
                result.errors.push(format!(
                    "Retry group '{}': stages must be consecutive (gap between index {} and {})",
                    group_name, window[0], window[1]
                ));
            }
        }
    }

    // Warn about retry groups defined but not used by any stage
    for group_name in config.retry_groups.keys() {
        if !group_ranges.contains_key(group_name.as_str()) {
            result.warnings.push(format!(
                "Retry group '{}' is defined but not referenced by any stage",
                group_name
            ));
        }
    }

    // Validate retry group verify configs
    for (group_name, group_config) in &config.retry_groups {
        if let Some(ref verify) = group_config.verify {
            if verify.system_prompt.trim().is_empty() {
                result.errors.push(format!(
                    "Retry group '{}': verify.system_prompt must not be empty",
                    group_name
                ));
            }
            if verify.user_prompt.trim().is_empty() {
                result.errors.push(format!(
                    "Retry group '{}': verify.user_prompt must not be empty",
                    group_name
                ));
            }
            if verify.timeout_secs == Some(0) {
                result.errors.push(format!(
                    "Retry group '{}': verify.timeout_secs must be greater than 0",
                    group_name
                ));
            }
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
