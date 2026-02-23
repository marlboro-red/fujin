use fujin_config::PipelineConfig;
use std::collections::{HashMap, HashSet, VecDeque};

/// Directed Acyclic Graph representing stage dependencies.
///
/// Built from a `PipelineConfig`. Stages without an explicit `dependencies`
/// field implicitly depend on the previous stage in YAML order (preserving
/// backward compatibility). An explicit `dependencies: []` means no dependencies.
#[derive(Debug)]
pub struct Dag {
    /// stage_id -> set of stage_ids it depends on (parents)
    dependencies: HashMap<String, HashSet<String>>,
    /// stage_id -> set of stage_ids that depend on it (children)
    dependents: HashMap<String, HashSet<String>>,
    /// stage_id -> index in config.stages
    stage_index: HashMap<String, usize>,
    /// Ordered list of stage IDs (matches config.stages order)
    stage_ids: Vec<String>,
}

impl Dag {
    /// Build a DAG from a pipeline config.
    ///
    /// Resolves implicit dependencies: when `dependencies` is `None`, the stage
    /// depends on the immediately preceding stage in YAML order. The first stage
    /// (or a stage with `dependencies: []`) has no dependencies.
    pub fn from_config(config: &PipelineConfig) -> Self {
        let mut dependencies: HashMap<String, HashSet<String>> = HashMap::new();
        let mut dependents: HashMap<String, HashSet<String>> = HashMap::new();
        let mut stage_index: HashMap<String, usize> = HashMap::new();
        let mut stage_ids: Vec<String> = Vec::new();

        for (i, stage) in config.stages.iter().enumerate() {
            stage_index.insert(stage.id.clone(), i);
            stage_ids.push(stage.id.clone());
            dependencies.insert(stage.id.clone(), HashSet::new());
            dependents.entry(stage.id.clone()).or_default();
        }

        let known_ids: HashSet<&str> = config.stages.iter().map(|s| s.id.as_str()).collect();

        for (i, stage) in config.stages.iter().enumerate() {
            let deps: Vec<String> = match &stage.dependencies {
                Some(deps) => deps.clone(),
                None => {
                    // Implicit: depend on previous stage in YAML order
                    if i > 0 {
                        vec![config.stages[i - 1].id.clone()]
                    } else {
                        vec![]
                    }
                }
            };

            for dep in deps {
                // Skip unknown dependency references (validation catches these;
                // silently inserting them would cause the pipeline to hang).
                if !known_ids.contains(dep.as_str()) {
                    continue;
                }
                dependencies.get_mut(&stage.id).unwrap().insert(dep.clone());
                dependents.entry(dep).or_default().insert(stage.id.clone());
            }
        }

        Dag {
            dependencies,
            dependents,
            stage_index,
            stage_ids,
        }
    }

    /// Return stage IDs whose dependencies are all satisfied (completed or skipped)
    /// and that are not already completed, skipped, or in-flight.
    pub fn ready_stages(
        &self,
        completed: &HashSet<String>,
        skipped: &HashSet<String>,
        in_flight: &HashSet<String>,
    ) -> Vec<String> {
        let satisfied: HashSet<&String> = completed.union(skipped).collect();
        let mut ready = Vec::new();

        for id in &self.stage_ids {
            if completed.contains(id) || skipped.contains(id) || in_flight.contains(id) {
                continue;
            }

            let deps = &self.dependencies[id];
            if deps.iter().all(|d| satisfied.contains(d)) {
                ready.push(id.clone());
            }
        }

        ready
    }

    /// Return all direct dependency stage IDs (parents) for a given stage.
    pub fn parents(&self, stage_id: &str) -> HashSet<String> {
        self.dependencies
            .get(stage_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Return all stages that depend on the given stage (children).
    pub fn children(&self, stage_id: &str) -> HashSet<String> {
        self.dependents
            .get(stage_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Return the index of a stage in the config.
    pub fn index_of(&self, stage_id: &str) -> Option<usize> {
        self.stage_index.get(stage_id).copied()
    }

    /// Return all stage IDs in config order.
    pub fn stage_ids(&self) -> &[String] {
        &self.stage_ids
    }

    /// Topological sort using Kahn's algorithm.
    ///
    /// Returns stage IDs in execution order, or an error if the graph has cycles.
    pub fn topological_sort(&self) -> Result<Vec<String>, String> {
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        for id in &self.stage_ids {
            in_degree.insert(id.as_str(), self.dependencies[id].len());
        }

        let mut queue: VecDeque<&str> = VecDeque::new();
        for (id, &deg) in &in_degree {
            if deg == 0 {
                queue.push_back(id);
            }
        }

        let mut sorted = Vec::new();
        while let Some(id) = queue.pop_front() {
            sorted.push(id.to_string());
            if let Some(children) = self.dependents.get(id) {
                for child in children {
                    let deg = in_degree.get_mut(child.as_str()).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(child.as_str());
                    }
                }
            }
        }

        if sorted.len() != self.stage_ids.len() {
            let remaining: Vec<&str> = self
                .stage_ids
                .iter()
                .filter(|id| !sorted.contains(id))
                .map(|s| s.as_str())
                .collect();
            Err(format!(
                "Circular dependency detected among stages: {:?}",
                remaining
            ))
        } else {
            Ok(sorted)
        }
    }

    /// Check if the DAG is linear (a single chain where every stage has at most
    /// one parent and one child, and there is at most one root node).
    ///
    /// A linear DAG is equivalent to the original sequential execution order.
    /// Disconnected graphs (multiple roots) are not linear — they benefit from
    /// parallel execution via the DAG scheduler.
    pub fn is_linear(&self) -> bool {
        let mut root_count = 0;
        for id in &self.stage_ids {
            if self.dependencies[id].is_empty() {
                root_count += 1;
                if root_count > 1 {
                    return false;
                }
            }
            if self.dependencies[id].len() > 1 {
                return false;
            }
            if self.dependents.get(id).map_or(0, |d| d.len()) > 1 {
                return false;
            }
        }
        true
    }

    /// Check if `ancestor` is a transitive dependency of `stage_id`.
    pub fn is_transitive_dependency(&self, stage_id: &str, ancestor: &str) -> bool {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(stage_id);

        while let Some(current) = queue.pop_front() {
            if !visited.insert(current) {
                continue;
            }
            if let Some(deps) = self.dependencies.get(current) {
                for dep in deps {
                    if dep == ancestor {
                        return true;
                    }
                    queue.push_back(dep.as_str());
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujin_config::PipelineConfig;

    fn make_config(yaml: &str) -> PipelineConfig {
        PipelineConfig::from_yaml(yaml).unwrap()
    }

    #[test]
    fn test_implicit_linear_deps() {
        let config = make_config(
            r#"
name: linear
stages:
  - id: a
    name: A
    system_prompt: sp
    user_prompt: up
  - id: b
    name: B
    system_prompt: sp
    user_prompt: up
  - id: c
    name: C
    system_prompt: sp
    user_prompt: up
"#,
        );
        let dag = Dag::from_config(&config);

        assert!(dag.parents("a").is_empty());
        assert_eq!(dag.parents("b"), HashSet::from(["a".to_string()]));
        assert_eq!(dag.parents("c"), HashSet::from(["b".to_string()]));
        assert!(dag.is_linear());
    }

    #[test]
    fn test_explicit_diamond_dag() {
        let config = make_config(
            r#"
name: diamond
stages:
  - id: analyze
    name: Analyze
    system_prompt: sp
    user_prompt: up
  - id: frontend
    name: Frontend
    depends_on: [analyze]
    system_prompt: sp
    user_prompt: up
  - id: backend
    name: Backend
    depends_on: [analyze]
    system_prompt: sp
    user_prompt: up
  - id: integrate
    name: Integration
    depends_on: [frontend, backend]
    system_prompt: sp
    user_prompt: up
"#,
        );
        let dag = Dag::from_config(&config);

        assert!(dag.parents("analyze").is_empty());
        assert_eq!(
            dag.parents("frontend"),
            HashSet::from(["analyze".to_string()])
        );
        assert_eq!(
            dag.parents("backend"),
            HashSet::from(["analyze".to_string()])
        );
        assert_eq!(
            dag.parents("integrate"),
            HashSet::from(["frontend".to_string(), "backend".to_string()])
        );
        assert!(!dag.is_linear());
    }

    #[test]
    fn test_ready_stages() {
        let config = make_config(
            r#"
name: diamond
stages:
  - id: analyze
    name: Analyze
    system_prompt: sp
    user_prompt: up
  - id: frontend
    name: Frontend
    depends_on: [analyze]
    system_prompt: sp
    user_prompt: up
  - id: backend
    name: Backend
    depends_on: [analyze]
    system_prompt: sp
    user_prompt: up
  - id: integrate
    name: Integration
    depends_on: [frontend, backend]
    system_prompt: sp
    user_prompt: up
"#,
        );
        let dag = Dag::from_config(&config);

        let empty = HashSet::new();
        let skipped = HashSet::new();

        // Initially only analyze is ready
        let ready = dag.ready_stages(&empty, &skipped, &empty);
        assert_eq!(ready, vec!["analyze"]);

        // After analyze completes, frontend and backend are ready
        let completed = HashSet::from(["analyze".to_string()]);
        let ready = dag.ready_stages(&completed, &skipped, &empty);
        assert_eq!(ready, vec!["frontend", "backend"]);

        // After frontend completes, backend still in flight, integrate not ready
        let completed = HashSet::from(["analyze".to_string(), "frontend".to_string()]);
        let in_flight = HashSet::from(["backend".to_string()]);
        let ready = dag.ready_stages(&completed, &skipped, &in_flight);
        assert!(ready.is_empty());

        // After both complete, integrate is ready
        let completed = HashSet::from([
            "analyze".to_string(),
            "frontend".to_string(),
            "backend".to_string(),
        ]);
        let ready = dag.ready_stages(&completed, &skipped, &empty);
        assert_eq!(ready, vec!["integrate"]);
    }

    #[test]
    fn test_ready_stages_with_skipped() {
        let config = make_config(
            r#"
name: skip
stages:
  - id: a
    name: A
    system_prompt: sp
    user_prompt: up
  - id: b
    name: B
    depends_on: [a]
    system_prompt: sp
    user_prompt: up
"#,
        );
        let dag = Dag::from_config(&config);

        // If a is skipped, b should be ready
        let completed = HashSet::new();
        let skipped = HashSet::from(["a".to_string()]);
        let ready = dag.ready_stages(&completed, &skipped, &HashSet::new());
        assert_eq!(ready, vec!["b"]);
    }

    #[test]
    fn test_topological_sort_linear() {
        let config = make_config(
            r#"
name: linear
stages:
  - id: a
    name: A
    system_prompt: sp
    user_prompt: up
  - id: b
    name: B
    system_prompt: sp
    user_prompt: up
  - id: c
    name: C
    system_prompt: sp
    user_prompt: up
"#,
        );
        let dag = Dag::from_config(&config);
        let sorted = dag.topological_sort().unwrap();
        assert_eq!(sorted, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_topological_sort_diamond() {
        let config = make_config(
            r#"
name: diamond
stages:
  - id: analyze
    name: Analyze
    system_prompt: sp
    user_prompt: up
  - id: frontend
    name: Frontend
    depends_on: [analyze]
    system_prompt: sp
    user_prompt: up
  - id: backend
    name: Backend
    depends_on: [analyze]
    system_prompt: sp
    user_prompt: up
  - id: integrate
    name: Integration
    depends_on: [frontend, backend]
    system_prompt: sp
    user_prompt: up
"#,
        );
        let dag = Dag::from_config(&config);
        let sorted = dag.topological_sort().unwrap();

        // analyze must be first, integrate must be last
        assert_eq!(sorted[0], "analyze");
        assert_eq!(sorted[3], "integrate");
        // frontend and backend can be in either order
        let middle: HashSet<&str> = sorted[1..3].iter().map(|s| s.as_str()).collect();
        assert!(middle.contains("frontend"));
        assert!(middle.contains("backend"));
    }

    #[test]
    fn test_depends_on_empty_means_no_deps() {
        let config = make_config(
            r#"
name: parallel
stages:
  - id: a
    name: A
    system_prompt: sp
    user_prompt: up
  - id: b
    name: B
    depends_on: []
    system_prompt: sp
    user_prompt: up
"#,
        );
        let dag = Dag::from_config(&config);

        // b has depends_on: [] so no dependencies (can run immediately)
        assert!(dag.parents("b").is_empty());

        // Both should be ready from the start
        let ready = dag.ready_stages(&HashSet::new(), &HashSet::new(), &HashSet::new());
        assert_eq!(ready, vec!["a", "b"]);
    }

    #[test]
    fn test_transitive_dependency() {
        let config = make_config(
            r#"
name: chain
stages:
  - id: a
    name: A
    system_prompt: sp
    user_prompt: up
  - id: b
    name: B
    depends_on: [a]
    system_prompt: sp
    user_prompt: up
  - id: c
    name: C
    depends_on: [b]
    system_prompt: sp
    user_prompt: up
"#,
        );
        let dag = Dag::from_config(&config);

        assert!(dag.is_transitive_dependency("c", "b"));
        assert!(dag.is_transitive_dependency("c", "a"));
        assert!(!dag.is_transitive_dependency("a", "c"));
        assert!(!dag.is_transitive_dependency("a", "b"));
    }

    #[test]
    fn test_is_linear_single_stage() {
        let config = make_config(
            r#"
name: single
stages:
  - id: a
    name: A
    system_prompt: sp
    user_prompt: up
"#,
        );
        let dag = Dag::from_config(&config);
        assert!(dag.is_linear());
    }

    #[test]
    fn test_index_of() {
        let config = make_config(
            r#"
name: test
stages:
  - id: a
    name: A
    system_prompt: sp
    user_prompt: up
  - id: b
    name: B
    system_prompt: sp
    user_prompt: up
"#,
        );
        let dag = Dag::from_config(&config);
        assert_eq!(dag.index_of("a"), Some(0));
        assert_eq!(dag.index_of("b"), Some(1));
        assert_eq!(dag.index_of("nonexistent"), None);
    }
}
