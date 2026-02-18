use fujin_config::PipelineConfig;
use std::path::{Path, PathBuf};

/// A discovered pipeline with its source path and parsed config.
#[derive(Debug, Clone)]
pub struct DiscoveredPipeline {
    /// Absolute path to the YAML file.
    pub source: PathBuf,
    /// Parsed pipeline config.
    pub config: PipelineConfig,
    /// Raw YAML content (needed for PipelineRunner).
    pub raw_yaml: String,
}

/// Discover pipeline YAML configs from multiple locations:
/// 1. Current directory (`*.yaml`)
/// 2. Global configs directory
///
/// Note: the templates directory is intentionally excluded — those are
/// scaffolding starters for `fujin init`, not runnable pipelines.
pub fn discover_pipelines() -> Vec<DiscoveredPipeline> {
    let mut pipelines = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    // 1. Current directory
    if let Ok(cwd) = std::env::current_dir() {
        scan_directory(&cwd, &mut pipelines, &mut seen_paths);
    }

    // 2. Global configs directory
    let configs_dir = fujin_core::paths::configs_dir();
    scan_directory(&configs_dir, &mut pipelines, &mut seen_paths);

    pipelines
}

/// Scan a directory for YAML files that parse as valid pipeline configs.
fn scan_directory(
    dir: &Path,
    pipelines: &mut Vec<DiscoveredPipeline>,
    seen: &mut std::collections::HashSet<PathBuf>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // Only process .yaml and .yml files
        let is_yaml = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "yaml" || e == "yml");

        if !is_yaml || !path.is_file() {
            continue;
        }

        // Canonicalize to deduplicate
        let canonical = match std::fs::canonicalize(&path) {
            Ok(p) => p,
            Err(_) => path.clone(),
        };

        if !seen.insert(canonical.clone()) {
            continue;
        }

        // Try to read and parse
        let raw_yaml = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let config = match PipelineConfig::from_yaml(&raw_yaml) {
            Ok(c) => c,
            Err(_) => continue,
        };

        pipelines.push(DiscoveredPipeline {
            source: canonical,
            config,
            raw_yaml,
        });
    }
}
