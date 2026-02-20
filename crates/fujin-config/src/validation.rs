use crate::pipeline_config::KNOWN_RUNTIMES;
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

    // Validate pipeline-level runtime
    if !KNOWN_RUNTIMES.contains(&config.runtime.as_str()) {
        result.warnings.push(format!(
            "Unknown pipeline runtime '{}' (known: {})",
            config.runtime,
            KNOWN_RUNTIMES.join(", ")
        ));
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

        // Validate per-stage runtime override
        if let Some(ref runtime) = stage.runtime {
            if !KNOWN_RUNTIMES.contains(&runtime.as_str()) {
                result.warnings.push(format!(
                    "{prefix}: unknown runtime '{}' (known: {})",
                    runtime,
                    KNOWN_RUNTIMES.join(", ")
                ));
            }
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

        // Validate exports config
        if let Some(ref exports) = stage.exports {
            if stage.is_command_stage() {
                result.warnings.push(format!(
                    "{prefix}: exports on a command stage — the agent won't have \
                     access to {{{{exports_file}}}}"
                ));
            }
            // keys is optional; no further validation needed since the
            // export file path is auto-computed by the pipeline runner
            let _ = exports;
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

    // Warn about command stages in retry groups with verify agents
    for (i, stage) in config.stages.iter().enumerate() {
        if let Some(ref group) = stage.retry_group {
            if stage.is_command_stage() {
                let has_verify = config
                    .retry_groups
                    .get(group.as_str())
                    .is_some_and(|g| g.verify.is_some());
                if has_verify {
                    result.warnings.push(format!(
                        "Stage {} ('{}'): command stage in retry group '{}' with verify agent; \
                         {{{{verify_feedback}}}} is not available in command stages",
                        i, stage.id, group
                    ));
                }
            }
        }
    }

    // Validate branching: when, branch, on_branch
    {
        // Collect stage IDs in order for forward-reference checking
        let stage_ids: Vec<&str> = config.stages.iter().map(|s| s.id.as_str()).collect();

        // Collect all routes defined by branch configs: maps branch_stage_id -> routes
        let mut defined_routes: std::collections::HashMap<&str, &Vec<String>> =
            std::collections::HashMap::new();
        for stage in &config.stages {
            if let Some(ref branch) = stage.branch {
                defined_routes.insert(stage.id.as_str(), &branch.routes);
            }
        }

        for (i, stage) in config.stages.iter().enumerate() {
            let prefix = format!("Stage {} ('{}')", i, stage.id);

            // A stage cannot have both branch and on_branch
            if stage.branch.is_some() && stage.on_branch.is_some() {
                result.errors.push(format!(
                    "{prefix}: cannot have both 'branch' and 'on_branch'"
                ));
            }

            // Validate 'when' condition
            if let Some(ref when) = stage.when {
                // when.stage must reference an earlier stage
                let ref_pos = stage_ids.iter().position(|id| *id == when.stage);
                match ref_pos {
                    None => {
                        result.errors.push(format!(
                            "{prefix}: when.stage '{}' does not reference a defined stage",
                            when.stage
                        ));
                    }
                    Some(pos) if pos >= i => {
                        result.errors.push(format!(
                            "{prefix}: when.stage '{}' must reference an earlier stage",
                            when.stage
                        ));
                    }
                    _ => {}
                }

                // output_matches must be a valid regex
                if regex::Regex::new(&when.output_matches).is_err() {
                    result.errors.push(format!(
                        "{prefix}: when.output_matches '{}' is not a valid regex",
                        when.output_matches
                    ));
                }
            }

            // Validate 'branch' config
            if let Some(ref branch) = stage.branch {
                if branch.routes.is_empty() {
                    result.errors.push(format!(
                        "{prefix}: branch.routes must not be empty"
                    ));
                }

                // branch.default must be one of the routes
                if let Some(ref default) = branch.default {
                    if !branch.routes.contains(default) {
                        result.errors.push(format!(
                            "{prefix}: branch.default '{}' is not in branch.routes",
                            default
                        ));
                    }
                }

                // Check that at least one downstream stage uses on_branch with a matching route
                let has_downstream = config.stages[i + 1..].iter().any(|s| {
                    s.on_branch.as_ref().is_some_and(|branches| {
                        branches.iter().any(|b| branch.routes.contains(b))
                    })
                });
                if !has_downstream && !branch.routes.is_empty() {
                    result.warnings.push(format!(
                        "{prefix}: branch defines routes {:?} but no downstream stage uses on_branch with any of them",
                        branch.routes
                    ));
                }
            }

            // Validate 'on_branch' values
            if let Some(ref on_branch) = stage.on_branch {
                // Each on_branch value must match a route in an earlier stage's branch
                for branch_name in on_branch {
                    let found = config.stages[..i].iter().any(|s| {
                        s.branch.as_ref().is_some_and(|b| b.routes.contains(branch_name))
                    });
                    if !found {
                        result.errors.push(format!(
                            "{prefix}: on_branch '{}' does not match any route in an earlier stage's branch",
                            branch_name
                        ));
                    }
                }
            }
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

    #[test]
    fn test_empty_pipeline_name() {
        let config = PipelineConfig::from_yaml(
            r#"
name: ""
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("name must not be empty")));
    }

    #[test]
    fn test_unknown_pipeline_runtime_warns() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
runtime: "unknown-runtime"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid()); // warnings don't make it invalid
        assert!(result.warnings.iter().any(|w| w.contains("Unknown pipeline runtime")));
    }

    #[test]
    fn test_empty_stage_id() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: ""
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("id must not be empty")));
    }

    #[test]
    fn test_empty_stage_name() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: ""
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("name must not be empty")));
    }

    #[test]
    fn test_unknown_stage_runtime_warns() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    runtime: "magic-runtime"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid());
        assert!(result.warnings.iter().any(|w| w.contains("unknown runtime 'magic-runtime'")));
    }

    #[test]
    fn test_empty_system_prompt() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: ""
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("system_prompt must not be empty")));
    }

    #[test]
    fn test_empty_user_prompt() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: ""
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("user_prompt must not be empty")));
    }

    #[test]
    fn test_unknown_tool_warns() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    allowed_tools:
      - "read"
      - "teleport"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid());
        assert!(result.warnings.iter().any(|w| w.contains("unknown tool 'teleport'")));
    }

    #[test]
    fn test_timeout_zero_is_error() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    timeout_secs: 0
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("timeout_secs must be greater than 0")));
    }

    #[test]
    fn test_command_stage_empty_command() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    commands:
      - "echo hello"
      - ""
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("command[1] must not be empty")));
    }

    #[test]
    fn test_prior_summary_in_first_stage_warns() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "Check this: {{prior_summary}}"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid());
        assert!(result.warnings.iter().any(|w| w.contains("prior_summary")));
    }

    #[test]
    fn test_retry_group_reference_not_defined() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    retry_group: "nonexistent"
  - id: "s2"
    name: "S2"
    system_prompt: "x"
    user_prompt: "y"
    retry_group: "nonexistent"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("not defined in retry_groups")));
    }

    #[test]
    fn test_single_stage_retry_group_without_verify_is_error() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
retry_groups:
  grp:
    max_retries: 3
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    retry_group: "grp"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("at least 2 stages")));
    }

    #[test]
    fn test_single_stage_retry_group_with_verify_is_ok() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
retry_groups:
  grp:
    max_retries: 3
    verify:
      system_prompt: "Check it"
      user_prompt: "Is it good?"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    retry_group: "grp"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_non_consecutive_retry_group_stages() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
retry_groups:
  grp:
    max_retries: 3
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    retry_group: "grp"
  - id: "s2"
    name: "S2"
    system_prompt: "x"
    user_prompt: "y"
  - id: "s3"
    name: "S3"
    system_prompt: "x"
    user_prompt: "y"
    retry_group: "grp"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("must be consecutive")));
    }

    #[test]
    fn test_unused_retry_group_warns() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
retry_groups:
  unused_grp:
    max_retries: 3
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid());
        assert!(result.warnings.iter().any(|w| w.contains("not referenced by any stage")));
    }

    #[test]
    fn test_verify_empty_prompts() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
retry_groups:
  grp:
    max_retries: 3
    verify:
      system_prompt: ""
      user_prompt: ""
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    retry_group: "grp"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("verify.system_prompt must not be empty")));
        assert!(result.errors.iter().any(|e| e.contains("verify.user_prompt must not be empty")));
    }

    #[test]
    fn test_verify_timeout_zero() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
retry_groups:
  grp:
    max_retries: 3
    verify:
      system_prompt: "check"
      user_prompt: "verify"
      timeout_secs: 0
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    retry_group: "grp"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("verify.timeout_secs must be greater than 0")));
    }

    #[test]
    fn test_is_valid_with_warnings_only() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
runtime: "unknown-runtime"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    allowed_tools:
      - "teleport"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid()); // warnings only
        assert!(!result.warnings.is_empty());
    }

    #[test]
    fn test_valid_command_stage() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    commands:
      - "echo hello"
      - "ls -la"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    // --- Branching validation tests ---

    #[test]
    fn test_when_references_nonexistent_stage() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    when:
      stage: "nonexistent"
      output_matches: ".*"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("does not reference a defined stage")));
    }

    #[test]
    fn test_when_references_later_stage() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    when:
      stage: "s2"
      output_matches: ".*"
    system_prompt: "x"
    user_prompt: "y"
  - id: "s2"
    name: "S2"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("must reference an earlier stage")));
    }

    #[test]
    fn test_when_invalid_regex() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
  - id: "s2"
    name: "S2"
    when:
      stage: "s1"
      output_matches: "[invalid"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("not a valid regex")));
    }

    #[test]
    fn test_when_valid() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "analyze"
    name: "Analyze"
    system_prompt: "x"
    user_prompt: "y"
  - id: "frontend"
    name: "Frontend"
    when:
      stage: "analyze"
      output_matches: "FRONTEND|FULLSTACK"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_branch_empty_routes() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    branch:
      prompt: "classify"
      routes: []
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("branch.routes must not be empty")));
    }

    #[test]
    fn test_branch_default_not_in_routes() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    branch:
      prompt: "classify"
      routes: [frontend, backend]
      default: fullstack
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("branch.default 'fullstack' is not in branch.routes")));
    }

    #[test]
    fn test_on_branch_without_matching_branch() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    on_branch: frontend
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("does not match any route")));
    }

    #[test]
    fn test_stage_with_both_branch_and_on_branch() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    branch:
      prompt: "classify"
      routes: [a, b]
    on_branch: a
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("cannot have both 'branch' and 'on_branch'")));
    }

    #[test]
    fn test_valid_branch_pipeline() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "analyze"
    name: "Analyze"
    system_prompt: "x"
    user_prompt: "y"
    branch:
      prompt: "classify"
      routes: [frontend, backend]
      default: frontend
  - id: "frontend-impl"
    name: "Frontend"
    on_branch: frontend
    system_prompt: "x"
    user_prompt: "y"
  - id: "backend-impl"
    name: "Backend"
    on_branch: backend
    system_prompt: "x"
    user_prompt: "y"
  - id: "review"
    name: "Review"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    // --- Exports validation tests ---

    #[test]
    fn test_exports_valid() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    exports:
      keys: [language, framework]
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_exports_on_command_stage_warns() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    commands:
      - "echo hi"
    exports:
      keys: [language]
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid()); // warning, not error
        assert!(result.warnings.iter().any(|w| w.contains("exports on a command stage")));
    }
}
