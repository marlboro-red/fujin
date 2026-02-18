use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("Agent runtime error: {message}")]
    AgentError { message: String },

    #[error("Agent runtime '{runtime}' is not available: {reason}")]
    RuntimeUnavailable { runtime: String, reason: String },

    #[error("Workspace error at '{path}': {message}")]
    WorkspaceError { path: PathBuf, message: String },

    #[error("Checkpoint error: {message}")]
    CheckpointError { message: String },

    #[error("Context building error: {message}")]
    ContextError { message: String },

    #[error("Template rendering error: {0}")]
    TemplateError(String),

    #[error("Config validation failed: {0}")]
    ValidationError(String),

    #[error("Pipeline was cancelled at stage '{stage_id}'")]
    Cancelled { stage_id: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Config error: {0}")]
    Config(#[from] fujin_config::ConfigError),
}

/// Result type alias for fujin-core operations.
pub type CoreResult<T> = Result<T, CoreError>;
