use crate::artifact::ArtifactSet;
use crate::stage::TokenUsage;
use std::time::Duration;

/// Structured events emitted by the pipeline runner during execution.
///
/// All pipeline progress is communicated via these events. The runner
/// always emits events through the provided `mpsc::UnboundedSender`.
/// Consumers (CLI, TUI) decide how to render them.
#[derive(Debug)]
pub enum PipelineEvent {
    /// Pipeline execution has started.
    PipelineStarted {
        pipeline_name: String,
        total_stages: usize,
        run_id: String,
        /// Default runtime name for the pipeline.
        runtime: String,
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
        /// Runtime name for this stage (e.g. "claude-code", "copilot-cli").
        runtime: String,
        /// Allowed tools for this stage.
        allowed_tools: Vec<String>,
        retry_group: Option<String>,
    },

    /// Context is being built for a stage (summarization, template rendering).
    ContextBuilding {
        stage_index: usize,
    },

    /// The fully rendered context/prompt that a stage will receive.
    StageContext {
        stage_index: usize,
        rendered_prompt: String,
        prior_summary: Option<String>,
    },

    /// The agent is actively running for a stage.
    AgentRunning {
        stage_index: usize,
        stage_id: String,
    },

    /// A CLI command is running in a command stage.
    CommandRunning {
        stage_index: usize,
        command_index: usize,
        command: String,
        total_commands: usize,
    },

    /// Output line from a CLI command.
    CommandOutput {
        stage_index: usize,
        line: String,
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
        /// Token usage aggregated by model name (sorted alphabetically).
        token_usage_by_model: Vec<(String, TokenUsage)>,
    },

    /// The pipeline failed.
    PipelineFailed {
        error: String,
    },

    /// The pipeline was cancelled by the user.
    PipelineCancelled {
        stage_index: usize,
    },

    /// A retry group is restarting after a stage failure.
    RetryGroupAttempt {
        group_name: String,
        attempt: u32,
        max_retries: u32,
        first_stage_index: usize,
        failed_stage_id: String,
        error: String,
    },

    /// A retry group has exhausted its retries and is asking the user to continue.
    RetryLimitReached {
        group_name: String,
        total_attempts: u32,
        response_tx: tokio::sync::oneshot::Sender<bool>,
    },

    /// Verification commands are running for a retry group.
    VerifyRunning {
        group_name: String,
        stage_index: usize,
    },

    /// All verification commands passed for a retry group.
    VerifyPassed {
        group_name: String,
        stage_index: usize,
    },

    /// A verification command failed, causing the retry group to loop.
    VerifyFailed {
        group_name: String,
        stage_index: usize,
        /// The verification agent's full response text.
        response: String,
    },

    /// Real-time activity from the verification agent.
    VerifyActivity {
        stage_index: usize,
        activity: String,
    },

    /// A stage was skipped due to a `when` condition or `on_branch` mismatch.
    StageSkipped {
        stage_index: usize,
        stage_id: String,
        reason: String,
    },

    /// A branch classifier is evaluating which route to take.
    BranchEvaluating {
        stage_index: usize,
        stage_id: String,
    },

    /// A branch route was selected by the classifier.
    BranchSelected {
        stage_index: usize,
        stage_id: String,
        selected_route: String,
        available_routes: Vec<String>,
    },
}
