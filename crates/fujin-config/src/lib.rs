mod includes;
mod pipeline_config;
mod validation;

pub use includes::resolve_includes;
pub use pipeline_config::{BranchConfig, ExportsConfig, IncludeConfig, PipelineConfig, RetryGroupConfig, StageConfig, SummarizerConfig, VerifyConfig, WhenCondition, KNOWN_RUNTIMES};
pub use validation::{validate, validate_includes, ValidationResult};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Failed to read config file '{path}': {source}")]
    FileRead {
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to parse YAML config: {0}")]
    YamlParse(#[from] serde_yml::Error),

    #[error("Config validation failed:\n{}", errors.join("\n"))]
    Validation {
        errors: Vec<String>,
        warnings: Vec<String>,
    },

    #[error("Include resolution failed for '{include_source}': {message}")]
    IncludeError {
        include_source: String,
        message: String,
    },
}
