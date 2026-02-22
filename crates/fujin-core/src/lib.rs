pub mod agent;
pub mod artifact;
pub mod checkpoint;
pub mod context;
pub mod copilot;
pub mod dag;
pub mod error;
pub mod event;
pub mod paths;
pub mod pipeline;
pub mod stage;
pub mod util;
pub mod workspace;
pub mod worktree;

pub use agent::{AgentOutput, AgentRuntime, ClaudeCodeRuntime};
pub use artifact::{ArtifactSet, FileChange, FileChangeKind};
pub use checkpoint::{Checkpoint, CheckpointManager, CheckpointSummary};
pub use dag::Dag;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_runtime_claude_code() {
        let runtime = create_runtime("claude-code").unwrap();
        assert_eq!(runtime.name(), "claude-code");
    }

    #[test]
    fn test_create_runtime_copilot_cli() {
        let runtime = create_runtime("copilot-cli").unwrap();
        assert_eq!(runtime.name(), "copilot-cli");
    }

    #[test]
    fn test_create_runtime_unknown() {
        let result = create_runtime("unknown-runtime");
        assert!(result.is_err());
        let err = result.err().unwrap();
        let msg = err.to_string();
        assert!(msg.contains("Unknown runtime"));
        assert!(msg.contains("unknown-runtime"));
    }
}
