use crate::artifact::ArtifactSet;
use crate::stage::TokenUsage;
use std::time::Duration;

/// Structured events emitted by the pipeline runner during execution.
///
/// All pipeline progress is communicated via these events. The runner
/// always emits events through the provided `mpsc::UnboundedSender`.
/// Consumers (CLI, TUI) decide how to render them.
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    /// Pipeline execution has started.
    PipelineStarted {
        pipeline_name: String,
        total_stages: usize,
        run_id: String,
    },

    /// Resuming from a checkpoint.
    Resuming {
        run_id: String,
        start_index: usize,
        total_stages: usize,
    },

    /// A stage has started executing.
    StageStarted {
        stage_index: usize,
        stage_id: String,
        stage_name: String,
        model: String,
    },

    /// Context is being built for a stage (summarization, template rendering).
    ContextBuilding {
        stage_index: usize,
    },

    /// The agent is actively running for a stage.
    AgentRunning {
        stage_index: usize,
        stage_id: String,
    },

    /// Periodic tick while the agent is running (sent every second).
    AgentTick {
        stage_index: usize,
        elapsed: Duration,
    },

    /// Real-time activity from the agent (tool use, text output).
    AgentActivity {
        stage_index: usize,
        activity: String,
    },

    /// A stage completed successfully.
    StageCompleted {
        stage_index: usize,
        stage_id: String,
        duration: Duration,
        artifacts: ArtifactSet,
        token_usage: Option<TokenUsage>,
    },

    /// A stage failed.
    StageFailed {
        stage_index: usize,
        stage_id: String,
        error: String,
        checkpoint_saved: bool,
    },

    /// The entire pipeline completed successfully.
    PipelineCompleted {
        total_duration: Duration,
        stages_completed: usize,
    },

    /// The pipeline failed.
    PipelineFailed {
        error: String,
    },

    /// The pipeline was cancelled by the user.
    PipelineCancelled {
        stage_index: usize,
    },
}
