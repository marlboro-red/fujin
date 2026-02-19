pub mod agent;
pub mod artifact;
pub mod checkpoint;
pub mod context;
pub mod copilot;
pub mod error;
pub mod event;
pub mod paths;
pub mod pipeline;
pub mod stage;
pub mod util;
pub mod workspace;

pub use agent::{AgentOutput, AgentRuntime, ClaudeCodeRuntime};
pub use artifact::{ArtifactSet, FileChange, FileChangeKind};
pub use checkpoint::{Checkpoint, CheckpointManager, CheckpointSummary};
pub use context::{ContextBuilder, StageContext};
pub use copilot::CopilotCliRuntime;
pub use error::{CoreError, CoreResult};
pub use event::PipelineEvent;
pub use pipeline::{PipelineRunner, RunOptions};
pub use stage::{StageResult, TokenUsage};
pub use workspace::Workspace;

/// Create an agent runtime by name.
///
/// Supported runtimes:
/// - `"claude-code"` — Claude Code CLI (`claude` binary)
/// - `"copilot-cli"` — GitHub Copilot CLI (`copilot` binary)
pub fn create_runtime(name: &str) -> Result<Box<dyn AgentRuntime>, CoreError> {
    match name {
        "claude-code" => Ok(Box::new(ClaudeCodeRuntime::new())),
        "copilot-cli" => Ok(Box::new(CopilotCliRuntime::new())),
        other => Err(CoreError::AgentError {
            message: format!(
                "Unknown runtime '{}'. Supported: claude-code, copilot-cli",
                other
            ),
        }),
    }
}
