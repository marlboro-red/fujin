mod pipeline_config;
mod validation;

pub use pipeline_config::{PipelineConfig, RetryGroupConfig, StageConfig, SummarizerConfig, VerifyConfig, KNOWN_RUNTIMES};
pub use validation::{validate, ValidationResult};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Failed to read config file '{path}': {source}")]
    FileRead {
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to parse YAML config: {0}")]
    YamlParse(#[from] serde_yml::Error),
}
