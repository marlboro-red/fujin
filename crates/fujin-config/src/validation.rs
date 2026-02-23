use crate::pipeline_config::KNOWN_RUNTIMES;
use crate::PipelineConfig;
use std::collections::{HashMap, HashSet, VecDeque};

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

    // Validate MCP server definitions
    for (name, server) in &config.mcp_servers {
        let has_command = server.command.as_ref().is_some_and(|c| !c.is_empty());
        let has_url = server.url.as_ref().is_some_and(|u| !u.is_empty());

        if !has_command && !has_url {
            result.errors.push(format!(
                "MCP server '{}': must specify either 'command' (stdio) or 'url' (HTTP/SSE)",
                name
            ));
        } else if has_command && has_url {
            result.errors.push(format!(
                "MCP server '{}': cannot specify both 'command' and 'url' (pick one transport)",
                name
            ));
        }

        // Empty string checks (non-None but empty)
        if server.command.as_ref().is_some_and(|c| c.trim().is_empty()) {
            result.errors.push(format!(
                "MCP server '{}': 'command' must not be empty",
                name
            ));
        }
        if server.url.as_ref().is_some_and(|u| u.trim().is_empty()) {
            result.errors.push(format!(
                "MCP server '{}': 'url' must not be empty",
                name
            ));
        }

        // Warn if args/env are set with url transport
        if has_url && !has_command {
            if !server.args.is_empty() {
                result.warnings.push(format!(
                    "MCP server '{}': 'args' is ignored for HTTP/SSE transport",
                    name
                ));
            }
            if !server.env.is_empty() {
                result.warnings.push(format!(
                    "MCP server '{}': 'env' is ignored for HTTP/SSE transport",
                    name
                ));
            }
        }

        // Warn if headers are set with stdio transport
        if has_command && !has_url && !server.headers.is_empty() {
            result.warnings.push(format!(
                "MCP server '{}': 'headers' is ignored for stdio transport",
                name
            ));
        }
    }

    // Validate per-stage MCP server references
    let mut referenced_mcp_servers = HashSet::new();
    for (i, stage) in config.stages.iter().enumerate() {
        let prefix = format!("Stage {} ('{}')", i, stage.id);

        for server_name in &stage.mcp_servers {
            referenced_mcp_servers.insert(server_name.as_str());
            if !config.mcp_servers.contains_key(server_name) {
                result.errors.push(format!(
                    "{prefix}: mcp_servers references undefined server '{server_name}'"
                ));
            }
        }

        if !stage.mcp_servers.is_empty() {
            if stage.is_command_stage() {
                result.warnings.push(format!(
                    "{prefix}: mcp_servers on a command stage (MCP servers are only used by agent runtimes)"
                ));
            }

            // Check if using copilot-cli runtime
            let effective_runtime = stage.runtime.as_deref().unwrap_or(&config.runtime);
            if effective_runtime == "copilot-cli" {
                result.warnings.push(format!(
                    "{prefix}: mcp_servers with copilot-cli runtime (MCP is not supported by copilot-cli)"
                ));
            }
        }
    }

    // Warn about defined but unreferenced MCP servers
    for server_name in config.mcp_servers.keys() {
        if !referenced_mcp_servers.contains(server_name.as_str()) {
            result.warnings.push(format!(
                "MCP server '{}' is defined but not referenced by any stage",
                server_name
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
                // when.stage must reference a defined stage (not self)
                if !stage_ids.contains(&when.stage.as_str()) {
                    result.errors.push(format!(
                        "{prefix}: when.stage '{}' does not reference a defined stage",
                        when.stage
                    ));
                } else if when.stage == stage.id {
                    result.errors.push(format!(
                        "{prefix}: when.stage '{}' cannot reference itself",
                        when.stage
                    ));
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

    // Validate depends_on
    {
        let stage_id_set: HashSet<&str> = config.stages.iter().map(|s| s.id.as_str()).collect();

        for (i, stage) in config.stages.iter().enumerate() {
            let prefix = format!("Stage {} ('{}')", i, stage.id);

            if let Some(ref deps) = stage.depends_on {
                for dep in deps {
                    // Each depends_on must reference a defined stage
                    if !stage_id_set.contains(dep.as_str()) {
                        result.errors.push(format!(
                            "{prefix}: depends_on references undefined stage '{dep}'"
                        ));
                    }

                    // No self-references
                    if dep == &stage.id {
                        result.errors.push(format!(
                            "{prefix}: depends_on cannot reference itself"
                        ));
                    }
                }
            }
        }

        // Build resolved dependency graph for cycle detection and transitive checks.
        // This mirrors DAG::from_config() logic: None = depend on previous, Some([]) = no deps.
        let mut resolved_deps: HashMap<&str, HashSet<&str>> = HashMap::new();
        let mut dependents: HashMap<&str, HashSet<&str>> = HashMap::new();

        for (i, stage) in config.stages.iter().enumerate() {
            let deps: HashSet<&str> = match &stage.depends_on {
                Some(deps) => deps.iter().map(|s| s.as_str()).collect(),
                None => {
                    if i > 0 {
                        let mut s = HashSet::new();
                        s.insert(config.stages[i - 1].id.as_str());
                        s
                    } else {
                        HashSet::new()
                    }
                }
            };
            for dep in &deps {
                dependents.entry(dep).or_default().insert(stage.id.as_str());
            }
            resolved_deps.insert(stage.id.as_str(), deps);
        }

        // Cycle detection via topological sort (Kahn's algorithm)
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        for stage in &config.stages {
            in_degree.insert(
                stage.id.as_str(),
                resolved_deps.get(stage.id.as_str()).map_or(0, |d| d.len()),
            );
        }

        let mut queue: VecDeque<&str> = VecDeque::new();
        for (&id, &deg) in &in_degree {
            if deg == 0 {
                queue.push_back(id);
            }
        }

        let mut sorted_count = 0usize;
        while let Some(id) = queue.pop_front() {
            sorted_count += 1;
            if let Some(children) = dependents.get(id) {
                for child in children {
                    let deg = in_degree.get_mut(child).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(child);
                    }
                }
            }
        }

        if sorted_count != config.stages.len() {
            let cycle_stages: Vec<&str> = config
                .stages
                .iter()
                .filter(|s| in_degree.get(s.id.as_str()).copied().unwrap_or(0) > 0)
                .map(|s| s.id.as_str())
                .collect();
            result.errors.push(format!(
                "Circular dependency detected among stages: {:?}",
                cycle_stages
            ));
        }

        // Helper: check if `ancestor` is a transitive dependency of `stage_id`
        let is_transitive_dep = |stage_id: &str, ancestor: &str| -> bool {
            let mut visited = HashSet::new();
            let mut q = VecDeque::new();
            q.push_back(stage_id);
            while let Some(current) = q.pop_front() {
                if !visited.insert(current) {
                    continue;
                }
                if let Some(deps) = resolved_deps.get(current) {
                    for dep in deps {
                        if *dep == ancestor {
                            return true;
                        }
                        q.push_back(dep);
                    }
                }
            }
            false
        };

        // when.stage must be a dependency (direct or transitive) of the current stage
        for (i, stage) in config.stages.iter().enumerate() {
            let prefix = format!("Stage {} ('{}')", i, stage.id);

            if let Some(ref when) = stage.when {
                if stage_id_set.contains(when.stage.as_str())
                    && !is_transitive_dep(&stage.id, &when.stage)
                {
                    result.errors.push(format!(
                        "{prefix}: when.stage '{}' must be a dependency (direct or transitive)",
                        when.stage
                    ));
                }
            }

            // on_branch: the stage that defines the matching branch must be a dependency
            if let Some(ref on_branch) = stage.on_branch {
                for branch_name in on_branch {
                    // Find the stage that defines this branch route
                    for prior_stage in &config.stages {
                        if let Some(ref branch) = prior_stage.branch {
                            if branch.routes.contains(branch_name)
                                && !is_transitive_dep(&stage.id, &prior_stage.id)
                            {
                                result.errors.push(format!(
                                    "{prefix}: on_branch '{}' requires stage '{}' (which defines the branch) to be a dependency",
                                    branch_name, prior_stage.id
                                ));
                            }
                        }
                    }
                }
            }
        }

        // Stages in the same retry_group must not have depends_on pointing outside the group
        let mut group_members: HashMap<&str, Vec<&str>> = HashMap::new();
        for stage in &config.stages {
            if let Some(ref group) = stage.retry_group {
                group_members.entry(group.as_str()).or_default().push(stage.id.as_str());
            }
        }

        for (group_name, members) in &group_members {
            let member_set: HashSet<&str> = members.iter().copied().collect();
            for &member_id in members {
                if let Some(deps) = resolved_deps.get(member_id) {
                    for dep in deps {
                        // deps within the group are fine (sequential within group)
                        // deps on stages *outside* the group are not allowed
                        // UNLESS it's the first stage in the group (which needs an external trigger)
                        if !member_set.contains(dep) && member_id != members[0] {
                            result.errors.push(format!(
                                "Stage '{}' in retry_group '{}': depends_on '{}' is outside the group (only the first stage in a retry group may depend on external stages)",
                                member_id, group_name, dep
                            ));
                        }
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

/// Validate include directives before resolution.
///
/// Checks structural issues that should be caught before attempting to
/// load and resolve included files.
pub fn validate_includes(config: &PipelineConfig) -> ValidationResult {
    let mut result = ValidationResult::default();

    if config.includes.is_empty() {
        return result;
    }

    let stage_ids: HashSet<&str> = config.stages.iter().map(|s| s.id.as_str()).collect();
    let mut seen_aliases = HashSet::new();

    for (i, include) in config.includes.iter().enumerate() {
        let prefix = format!("Include {} ('{}')", i, include.source);

        // source must not be empty
        if include.source.trim().is_empty() {
            result.errors.push(format!("{prefix}: source must not be empty"));
        }

        // alias must not be empty
        if include.alias.trim().is_empty() {
            result.errors.push(format!("{prefix}: 'as' alias must not be empty"));
        }

        // alias must be a valid identifier (alphanumeric + underscore, no dots)
        if !include.alias.is_empty()
            && !include
                .alias
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        {
            result.errors.push(format!(
                "{prefix}: alias '{}' must contain only alphanumeric characters, underscores, or hyphens",
                include.alias
            ));
        }

        // No duplicate aliases
        if !include.alias.is_empty() && !seen_aliases.insert(&include.alias) {
            result.errors.push(format!(
                "{prefix}: duplicate alias '{}'",
                include.alias
            ));
        }

        // depends_on entries must reference stage IDs defined in the parent's stages
        for dep in &include.depends_on {
            if !stage_ids.contains(dep.as_str()) {
                result.errors.push(format!(
                    "{prefix}: depends_on references undefined parent stage '{dep}'"
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
        // when.stage 's2' is not a transitive dependency of 's1' (s1 implicitly depends on nothing)
        assert!(result.errors.iter().any(|e| e.contains("must be a dependency")));
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

    // --- depends_on validation tests ---

    #[test]
    fn test_depends_on_valid_diamond() {
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
    depends_on: [analyze]
    system_prompt: "x"
    user_prompt: "y"
  - id: "backend"
    name: "Backend"
    depends_on: [analyze]
    system_prompt: "x"
    user_prompt: "y"
  - id: "integrate"
    name: "Integration"
    depends_on: [frontend, backend]
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_depends_on_undefined_stage() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    depends_on: [nonexistent]
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("undefined stage 'nonexistent'")));
    }

    #[test]
    fn test_depends_on_self_reference() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    depends_on: [s1]
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("cannot reference itself")));
    }

    #[test]
    fn test_depends_on_circular_dependency() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "a"
    name: "A"
    depends_on: [b]
    system_prompt: "x"
    user_prompt: "y"
  - id: "b"
    name: "B"
    depends_on: [a]
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("Circular dependency")));
    }

    #[test]
    fn test_depends_on_when_stage_must_be_dependency() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "a"
    name: "A"
    system_prompt: "x"
    user_prompt: "y"
  - id: "b"
    name: "B"
    depends_on: []
    when:
      stage: "a"
      output_matches: ".*"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("must be a dependency")));
    }

    #[test]
    fn test_depends_on_empty_list_allows_immediate_start() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "a"
    name: "A"
    depends_on: []
    system_prompt: "x"
    user_prompt: "y"
  - id: "b"
    name: "B"
    depends_on: []
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_depends_on_backward_compatible_no_depends() {
        // Pipeline without any depends_on should still be valid (implicit linear chain)
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
    system_prompt: "x"
    user_prompt: "y"
  - id: "s3"
    name: "S3"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_depends_on_with_valid_when_as_dependency() {
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
    depends_on: [analyze]
    when:
      stage: "analyze"
      output_matches: "FRONTEND"
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    // --- validate_includes tests ---

    #[test]
    fn test_validate_includes_empty_source() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
includes:
  - source: ""
    as: be
stages:
  - id: s1
    name: S1
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate_includes(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("source must not be empty")));
    }

    #[test]
    fn test_validate_includes_empty_alias() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
includes:
  - source: backend.yaml
    as: ""
stages:
  - id: s1
    name: S1
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate_includes(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("alias must not be empty")));
    }

    #[test]
    fn test_validate_includes_invalid_alias_chars() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
includes:
  - source: backend.yaml
    as: "be.end"
stages:
  - id: s1
    name: S1
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate_includes(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("must contain only alphanumeric")));
    }

    #[test]
    fn test_validate_includes_duplicate_alias() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
includes:
  - source: a.yaml
    as: be
  - source: b.yaml
    as: be
stages:
  - id: s1
    name: S1
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate_includes(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("duplicate alias")));
    }

    #[test]
    fn test_validate_includes_depends_on_undefined_parent_stage() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
includes:
  - source: backend.yaml
    as: be
    depends_on: [nonexistent]
stages:
  - id: s1
    name: S1
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate_includes(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("undefined parent stage 'nonexistent'")));
    }

    #[test]
    fn test_validate_includes_valid() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
includes:
  - source: backend.yaml
    as: be
    depends_on: [s1]
    vars:
      db_url: "localhost"
stages:
  - id: s1
    name: S1
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate_includes(&config);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_validate_includes_no_includes() {
        let config = minimal_config();
        let result = validate_includes(&config);
        assert!(result.is_valid());
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_validate_includes_hyphen_and_underscore_alias() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
includes:
  - source: a.yaml
    as: my-backend
  - source: b.yaml
    as: my_frontend
stages:
  - id: s1
    name: S1
    system_prompt: "x"
    user_prompt: "y"
"#,
        )
        .unwrap();
        let result = validate_includes(&config);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    // --- MCP server validation tests ---

    #[test]
    fn test_mcp_server_missing_transport() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
mcp_servers:
  broken:
    args: ["-y"]
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    mcp_servers: [broken]
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("must specify either 'command'")));
    }

    #[test]
    fn test_mcp_server_both_transports() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
mcp_servers:
  broken:
    command: npx
    url: https://example.com/mcp
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    mcp_servers: [broken]
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("cannot specify both")));
    }

    #[test]
    fn test_mcp_server_undefined_reference() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    mcp_servers: [nonexistent]
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("undefined server 'nonexistent'")));
    }

    #[test]
    fn test_mcp_server_valid() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
mcp_servers:
  database:
    command: npx
    args: [-y, "@modelcontextprotocol/server-postgres"]
    env:
      DATABASE_URL: postgresql://localhost/mydb
  api-docs:
    url: https://api-docs.example.com/mcp
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    mcp_servers: [database, api-docs]
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid(), "Errors: {:?}", result.errors);
    }

    #[test]
    fn test_mcp_server_on_command_stage_warns() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
mcp_servers:
  db:
    command: npx
stages:
  - id: "s1"
    name: "S1"
    commands:
      - "echo hi"
    mcp_servers: [db]
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid());
        assert!(result.warnings.iter().any(|w| w.contains("mcp_servers on a command stage")));
    }

    #[test]
    fn test_mcp_server_on_copilot_warns() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
mcp_servers:
  db:
    command: npx
stages:
  - id: "s1"
    name: "S1"
    runtime: copilot-cli
    system_prompt: "x"
    user_prompt: "y"
    mcp_servers: [db]
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid());
        assert!(result.warnings.iter().any(|w| w.contains("copilot-cli")));
    }

    #[test]
    fn test_mcp_server_unreferenced_warns() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
mcp_servers:
  unused_db:
    command: npx
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
    fn test_mcp_server_url_with_args_warns() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
mcp_servers:
  api:
    url: https://example.com/mcp
    args: ["-y"]
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    mcp_servers: [api]
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid());
        assert!(result.warnings.iter().any(|w| w.contains("'args' is ignored")));
    }

    #[test]
    fn test_mcp_server_headers_on_stdio_warns() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
mcp_servers:
  db:
    command: npx
    headers:
      Authorization: "Bearer token"
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    mcp_servers: [db]
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(result.is_valid());
        assert!(result.warnings.iter().any(|w| w.contains("'headers' is ignored")));
    }

    #[test]
    fn test_mcp_server_empty_command_string() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
mcp_servers:
  bad:
    command: ""
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    mcp_servers: [bad]
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("'command' must not be empty")));
    }

    #[test]
    fn test_mcp_server_empty_url_string() {
        let config = PipelineConfig::from_yaml(
            r#"
name: "Test"
mcp_servers:
  bad:
    url: ""
stages:
  - id: "s1"
    name: "S1"
    system_prompt: "x"
    user_prompt: "y"
    mcp_servers: [bad]
"#,
        )
        .unwrap();
        let result = validate(&config);
        assert!(!result.is_valid());
        assert!(result.errors.iter().any(|e| e.contains("'url' must not be empty")));
    }
}
