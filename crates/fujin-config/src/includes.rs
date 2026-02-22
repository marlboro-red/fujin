use crate::{ConfigError, PipelineConfig};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Resolve all `includes` directives in a pipeline config, producing a flat
/// config with all included stages merged in. Included stages have their IDs
/// prefixed with the include alias (e.g. `be.build`).
///
/// This must be called **before** validation and DAG construction.
pub fn resolve_includes(
    config: PipelineConfig,
    base_dir: &Path,
) -> Result<PipelineConfig, ConfigError> {
    if config.includes.is_empty() {
        return Ok(config);
    }

    let canonical_base = std::fs::canonicalize(base_dir).unwrap_or_else(|_| base_dir.to_path_buf());
    let mut seen = HashSet::new();

    resolve_recursive(config, &canonical_base, &mut seen)
}

fn resolve_recursive(
    mut config: PipelineConfig,
    base_dir: &Path,
    seen: &mut HashSet<PathBuf>,
) -> Result<PipelineConfig, ConfigError> {
    if config.includes.is_empty() {
        return Ok(config);
    }

    let includes = std::mem::take(&mut config.includes);
    let mut all_included_stages = Vec::new();
    let mut merged_retry_groups = config.retry_groups.clone();

    for include in &includes {
        // 1. Resolve source path
        let source_path = base_dir.join(&include.source);
        let canonical = std::fs::canonicalize(&source_path).map_err(|e| {
            ConfigError::IncludeError {
                include_source: include.source.clone(),
                message: format!("Cannot resolve path '{}': {}", source_path.display(), e),
            }
        })?;

        // 2. Circular include detection
        if !seen.insert(canonical.clone()) {
            return Err(ConfigError::IncludeError {
                include_source: include.source.clone(),
                message: format!(
                    "Circular include detected: '{}' was already included",
                    canonical.display()
                ),
            });
        }

        // 3. Load + parse the included file
        let child_yaml = std::fs::read_to_string(&canonical).map_err(|e| {
            ConfigError::IncludeError {
                include_source: include.source.clone(),
                message: format!("Failed to read '{}': {}", canonical.display(), e),
            }
        })?;

        let child_config = PipelineConfig::from_yaml(&child_yaml).map_err(|e| {
            ConfigError::IncludeError {
                include_source: include.source.clone(),
                message: format!("Failed to parse: {e}"),
            }
        })?;

        // 4. Recursively resolve nested includes
        let child_base = canonical.parent().unwrap_or(base_dir);
        let child_config = resolve_recursive(child_config, child_base, seen)?;

        // 5. Process the included pipeline
        let processed = process_include(&config, &include.alias, &include.depends_on, &include.vars, &child_config)?;

        // Merge retry groups (prefixed)
        for (group_name, group_config) in &child_config.retry_groups {
            merged_retry_groups.insert(
                format!("{}.{group_name}", include.alias),
                group_config.clone(),
            );
        }

        all_included_stages.extend(processed);
    }

    // Assemble final config: parent stages + included stages
    let mut final_stages = std::mem::take(&mut config.stages);
    final_stages.extend(all_included_stages);

    config.stages = final_stages;
    config.retry_groups = merged_retry_groups;

    Ok(config)
}

/// Process a single include: prefix IDs, wire dependencies, substitute variables.
fn process_include(
    parent_config: &PipelineConfig,
    alias: &str,
    include_depends_on: &[String],
    include_vars: &HashMap<String, String>,
    child_config: &PipelineConfig,
) -> Result<Vec<crate::StageConfig>, ConfigError> {
    let child_ids: HashSet<&str> = child_config.stages.iter().map(|s| s.id.as_str()).collect();

    // Collect original IDs in order (for implicit dep resolution)
    let original_ids: Vec<&str> = child_config.stages.iter().map(|s| s.id.as_str()).collect();

    // Build merged variable set: child defaults + include.vars overrides
    let mut merged_vars = child_config.variables.clone();
    for (k, v) in include_vars {
        merged_vars.insert(k.clone(), v.clone());
    }

    let mut result_stages = Vec::with_capacity(child_config.stages.len());

    for (i, stage) in child_config.stages.iter().enumerate() {
        let mut stage = stage.clone();

        // Prefix stage ID
        stage.id = format!("{alias}.{}", stage.id);

        // Handle depends_on
        match &stage.depends_on {
            None => {
                // Implicit dep: depends on previous stage in the include, or include-level depends_on for first
                if i == 0 {
                    stage.depends_on = Some(include_depends_on.to_vec());
                } else {
                    stage.depends_on = Some(vec![format!("{alias}.{}", original_ids[i - 1])]);
                }
            }
            Some(deps) if deps.is_empty() => {
                // Explicit root stage: wire include-level depends_on
                stage.depends_on = Some(include_depends_on.to_vec());
            }
            Some(deps) => {
                let mut new_deps: Vec<String> = deps
                    .iter()
                    .map(|d| {
                        if child_ids.contains(d.as_str()) {
                            format!("{alias}.{d}")
                        } else {
                            d.clone()
                        }
                    })
                    .collect();

                // If all deps were external (i.e. root-like), also add include-level deps
                let all_external = deps.iter().all(|d| !child_ids.contains(d.as_str()));
                if all_external {
                    for dep in include_depends_on {
                        if !new_deps.contains(dep) {
                            new_deps.push(dep.clone());
                        }
                    }
                }

                stage.depends_on = Some(new_deps);
            }
        }

        // Prefix retry_group
        if let Some(ref group) = stage.retry_group {
            stage.retry_group = Some(format!("{alias}.{group}"));
        }

        // Prefix when.stage (if it references a child stage)
        if let Some(ref mut when) = stage.when {
            if child_ids.contains(when.stage.as_str()) {
                when.stage = format!("{alias}.{}", when.stage);
            }
        }

        // Prefix on_branch references that match child stage branches
        // (on_branch values are route names, not stage IDs — they don't need prefixing)

        // Prefix branch config route refs in on_branch — actually routes are just strings,
        // not stage IDs. They don't need prefixing. Cross-boundary branch routing
        // works because the route names themselves are unique.

        // Runtime propagation
        if stage.runtime.is_none() && child_config.runtime != parent_config.runtime {
            stage.runtime = Some(child_config.runtime.clone());
        }

        // Variable substitution in prompts
        substitute_vars(&mut stage.system_prompt, &merged_vars);
        substitute_vars(&mut stage.user_prompt, &merged_vars);

        // Substitute in commands
        if let Some(ref mut commands) = stage.commands {
            for cmd in commands.iter_mut() {
                substitute_vars(cmd, &merged_vars);
            }
        }

        result_stages.push(stage);
    }

    Ok(result_stages)
}

/// Replace `{{key}}` patterns in text with values from the variable map.
fn substitute_vars(text: &mut String, vars: &HashMap<String, String>) {
    for (key, value) in vars {
        let pattern = format!("{{{{{key}}}}}");
        if text.contains(&pattern) {
            *text = text.replace(&pattern, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a temp dir with YAML files and return the dir path.
    fn setup_temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn test_resolve_no_includes() {
        let config = PipelineConfig::from_yaml(
            r#"
name: simple
stages:
  - id: s1
    name: S1
    system_prompt: "sp"
    user_prompt: "up"
"#,
        )
        .unwrap();

        let result = resolve_includes(config.clone(), Path::new(".")).unwrap();
        assert_eq!(result.stages.len(), 1);
        assert_eq!(result.stages[0].id, "s1");
    }

    #[test]
    fn test_resolve_single_include() {
        let dir = setup_temp_dir();

        // Write child pipeline
        fs::write(
            dir.path().join("backend.yaml"),
            r#"
name: backend
stages:
  - id: build
    name: Build
    depends_on: []
    system_prompt: "Build it"
    user_prompt: "Go"
  - id: deploy
    name: Deploy
    system_prompt: "Deploy it"
    user_prompt: "Go"
"#,
        )
        .unwrap();

        // Write parent pipeline
        let parent_yaml = r#"
name: fullstack
includes:
  - source: backend.yaml
    as: be
    depends_on: [analyze]
stages:
  - id: analyze
    name: Analyze
    depends_on: []
    system_prompt: "Analyze"
    user_prompt: "Go"
  - id: integrate
    name: Integrate
    depends_on: [be.deploy]
    system_prompt: "Integrate"
    user_prompt: "Go"
"#;

        let config = PipelineConfig::from_yaml(parent_yaml).unwrap();
        let resolved = resolve_includes(config, dir.path()).unwrap();

        assert_eq!(resolved.stages.len(), 4);
        assert_eq!(resolved.stages[0].id, "analyze");
        assert_eq!(resolved.stages[1].id, "integrate");
        assert_eq!(resolved.stages[2].id, "be.build");
        assert_eq!(resolved.stages[3].id, "be.deploy");

        // be.build should depend on [analyze] (from include-level depends_on)
        assert_eq!(
            resolved.stages[2].depends_on,
            Some(vec!["analyze".to_string()])
        );
        // be.deploy should depend on be.build (implicit→explicit)
        assert_eq!(
            resolved.stages[3].depends_on,
            Some(vec!["be.build".to_string()])
        );
    }

    #[test]
    fn test_resolve_multiple_includes() {
        let dir = setup_temp_dir();

        fs::write(
            dir.path().join("backend.yaml"),
            r#"
name: backend
stages:
  - id: build
    name: Build BE
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
"#,
        )
        .unwrap();

        fs::write(
            dir.path().join("frontend.yaml"),
            r#"
name: frontend
stages:
  - id: build
    name: Build FE
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
"#,
        )
        .unwrap();

        let parent_yaml = r#"
name: fullstack
includes:
  - source: backend.yaml
    as: be
    depends_on: [analyze]
  - source: frontend.yaml
    as: fe
    depends_on: [analyze]
stages:
  - id: analyze
    name: Analyze
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
  - id: integrate
    name: Integrate
    depends_on: [be.build, fe.build]
    system_prompt: "sp"
    user_prompt: "up"
"#;

        let config = PipelineConfig::from_yaml(parent_yaml).unwrap();
        let resolved = resolve_includes(config, dir.path()).unwrap();

        assert_eq!(resolved.stages.len(), 4);
        // Check prefixed IDs
        let ids: Vec<&str> = resolved.stages.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"be.build"));
        assert!(ids.contains(&"fe.build"));
    }

    #[test]
    fn test_resolve_nested_includes() {
        let dir = setup_temp_dir();

        // Leaf: database stages
        fs::write(
            dir.path().join("database.yaml"),
            r#"
name: database
stages:
  - id: migrate
    name: Migrate
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
"#,
        )
        .unwrap();

        // Mid-level: backend includes database
        fs::write(
            dir.path().join("backend.yaml"),
            r#"
name: backend
includes:
  - source: database.yaml
    as: db
    depends_on: [build]
stages:
  - id: build
    name: Build
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
"#,
        )
        .unwrap();

        // Top-level: fullstack includes backend
        let parent_yaml = r#"
name: fullstack
includes:
  - source: backend.yaml
    as: be
stages:
  - id: start
    name: Start
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
"#;

        let config = PipelineConfig::from_yaml(parent_yaml).unwrap();
        let resolved = resolve_includes(config, dir.path()).unwrap();

        let ids: Vec<&str> = resolved.stages.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"start"));
        assert!(ids.contains(&"be.build"));
        // Nested include: database.migrate gets prefixed first as db.migrate by backend,
        // then again as be.db.migrate by the parent
        assert!(ids.contains(&"be.db.migrate"));
    }

    #[test]
    fn test_circular_include_detected() {
        let dir = setup_temp_dir();

        fs::write(
            dir.path().join("a.yaml"),
            r#"
name: a
includes:
  - source: b.yaml
    as: b
stages:
  - id: s1
    name: S1
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
"#,
        )
        .unwrap();

        fs::write(
            dir.path().join("b.yaml"),
            r#"
name: b
includes:
  - source: a.yaml
    as: a
stages:
  - id: s1
    name: S1
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
"#,
        )
        .unwrap();

        let parent_yaml = r#"
name: top
includes:
  - source: a.yaml
    as: inc_a
stages:
  - id: root
    name: Root
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
"#;

        let config = PipelineConfig::from_yaml(parent_yaml).unwrap();
        let result = resolve_includes(config, dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Circular include"), "Got: {err}");
    }

    #[test]
    fn test_include_vars_override() {
        let dir = setup_temp_dir();

        fs::write(
            dir.path().join("child.yaml"),
            r#"
name: child
variables:
  db_url: "localhost"
  port: "3000"
stages:
  - id: build
    name: Build
    depends_on: []
    system_prompt: "Connect to {{db_url}} on port {{port}}"
    user_prompt: "Go"
"#,
        )
        .unwrap();

        let parent_yaml = r#"
name: parent
includes:
  - source: child.yaml
    as: ch
    vars:
      db_url: "prod-db.example.com"
stages: []
"#;

        let config = PipelineConfig::from_yaml(parent_yaml).unwrap();
        let resolved = resolve_includes(config, dir.path()).unwrap();

        assert_eq!(resolved.stages.len(), 1);
        assert_eq!(resolved.stages[0].id, "ch.build");
        // db_url should be overridden, port should keep default
        assert_eq!(
            resolved.stages[0].system_prompt,
            "Connect to prod-db.example.com on port 3000"
        );
    }

    #[test]
    fn test_include_runtime_propagation() {
        let dir = setup_temp_dir();

        fs::write(
            dir.path().join("child.yaml"),
            r#"
name: child
runtime: copilot-cli
stages:
  - id: build
    name: Build
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
  - id: test
    name: Test
    runtime: claude-code
    system_prompt: "sp"
    user_prompt: "up"
"#,
        )
        .unwrap();

        let parent_yaml = r#"
name: parent
runtime: claude-code
includes:
  - source: child.yaml
    as: ch
stages: []
"#;

        let config = PipelineConfig::from_yaml(parent_yaml).unwrap();
        let resolved = resolve_includes(config, dir.path()).unwrap();

        // ch.build should get runtime = copilot-cli (from child pipeline default, different from parent)
        assert_eq!(
            resolved.stages[0].runtime,
            Some("copilot-cli".to_string())
        );
        // ch.test already has explicit runtime override, should keep it
        assert_eq!(
            resolved.stages[1].runtime,
            Some("claude-code".to_string())
        );
    }

    #[test]
    fn test_implicit_deps_converted() {
        let dir = setup_temp_dir();

        fs::write(
            dir.path().join("child.yaml"),
            r#"
name: child
stages:
  - id: first
    name: First
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
  - id: second
    name: Second
    system_prompt: "sp"
    user_prompt: "up"
  - id: third
    name: Third
    system_prompt: "sp"
    user_prompt: "up"
"#,
        )
        .unwrap();

        let parent_yaml = r#"
name: parent
includes:
  - source: child.yaml
    as: ch
    depends_on: [setup]
stages:
  - id: setup
    name: Setup
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
"#;

        let config = PipelineConfig::from_yaml(parent_yaml).unwrap();
        let resolved = resolve_includes(config, dir.path()).unwrap();

        // ch.first: root stage → gets include depends_on [setup]
        let first = resolved.stages.iter().find(|s| s.id == "ch.first").unwrap();
        assert_eq!(first.depends_on, Some(vec!["setup".to_string()]));

        // ch.second: implicit dep → ch.first
        let second = resolved.stages.iter().find(|s| s.id == "ch.second").unwrap();
        assert_eq!(second.depends_on, Some(vec!["ch.first".to_string()]));

        // ch.third: implicit dep → ch.second
        let third = resolved.stages.iter().find(|s| s.id == "ch.third").unwrap();
        assert_eq!(third.depends_on, Some(vec!["ch.second".to_string()]));
    }

    #[test]
    fn test_retry_group_prefixing() {
        let dir = setup_temp_dir();

        fs::write(
            dir.path().join("child.yaml"),
            r#"
name: child
retry_groups:
  fix:
    max_retries: 3
stages:
  - id: code
    name: Code
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
    retry_group: fix
  - id: test
    name: Test
    system_prompt: "sp"
    user_prompt: "up"
    retry_group: fix
"#,
        )
        .unwrap();

        let parent_yaml = r#"
name: parent
includes:
  - source: child.yaml
    as: ch
stages: []
"#;

        let config = PipelineConfig::from_yaml(parent_yaml).unwrap();
        let resolved = resolve_includes(config, dir.path()).unwrap();

        // Retry groups should be prefixed
        assert!(resolved.retry_groups.contains_key("ch.fix"));
        assert!(!resolved.retry_groups.contains_key("fix"));

        // Stage retry_group refs should be prefixed
        assert_eq!(
            resolved.stages[0].retry_group,
            Some("ch.fix".to_string())
        );
        assert_eq!(
            resolved.stages[1].retry_group,
            Some("ch.fix".to_string())
        );
    }

    #[test]
    fn test_when_stage_prefixing() {
        let dir = setup_temp_dir();

        fs::write(
            dir.path().join("child.yaml"),
            r#"
name: child
stages:
  - id: analyze
    name: Analyze
    depends_on: []
    system_prompt: "sp"
    user_prompt: "up"
  - id: frontend
    name: Frontend
    depends_on: [analyze]
    when:
      stage: analyze
      output_matches: "FRONTEND"
    system_prompt: "sp"
    user_prompt: "up"
"#,
        )
        .unwrap();

        let parent_yaml = r#"
name: parent
includes:
  - source: child.yaml
    as: ch
stages: []
"#;

        let config = PipelineConfig::from_yaml(parent_yaml).unwrap();
        let resolved = resolve_includes(config, dir.path()).unwrap();

        let frontend = resolved.stages.iter().find(|s| s.id == "ch.frontend").unwrap();
        assert_eq!(frontend.when.as_ref().unwrap().stage, "ch.analyze");
    }

    #[test]
    fn test_include_source_not_found() {
        let dir = setup_temp_dir();

        let parent_yaml = r#"
name: parent
includes:
  - source: nonexistent.yaml
    as: ne
stages: []
"#;

        let config = PipelineConfig::from_yaml(parent_yaml).unwrap();
        let result = resolve_includes(config, dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("nonexistent.yaml"), "Got: {err}");
    }

    #[test]
    fn test_variable_substitution_in_commands() {
        let dir = setup_temp_dir();

        fs::write(
            dir.path().join("child.yaml"),
            r#"
name: child
variables:
  env: "dev"
stages:
  - id: deploy
    name: Deploy
    depends_on: []
    commands:
      - "deploy --env={{env}}"
"#,
        )
        .unwrap();

        let parent_yaml = r#"
name: parent
includes:
  - source: child.yaml
    as: ch
    vars:
      env: "production"
stages: []
"#;

        let config = PipelineConfig::from_yaml(parent_yaml).unwrap();
        let resolved = resolve_includes(config, dir.path()).unwrap();

        let deploy = resolved.stages.iter().find(|s| s.id == "ch.deploy").unwrap();
        assert_eq!(
            deploy.commands.as_ref().unwrap()[0],
            "deploy --env=production"
        );
    }
}
