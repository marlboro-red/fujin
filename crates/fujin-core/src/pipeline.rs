use crate::agent::{AgentRuntime, ClaudeCodeRuntime};
use crate::checkpoint::CheckpointManager;
use crate::context::ContextBuilder;
use crate::error::{CoreError, CoreResult};
use crate::event::PipelineEvent;
use crate::stage::StageResult;
use crate::workspace::Workspace;
use chrono::Utc;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::info;
use fujin_config::PipelineConfig;

/// Options for running a pipeline.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Whether to attempt resuming from a checkpoint.
    pub resume: bool,

    /// If true, validate and emit a dry-run event without executing.
    pub dry_run: bool,
}

/// Executes a pipeline configuration.
///
/// All progress is communicated via `PipelineEvent`s through the event
/// sender. The runner never prints to stdout/stderr directly — consumers
/// (CLI, TUI) decide how to display events.
pub struct PipelineRunner {
    config: PipelineConfig,
    config_yaml: String,
    runtime: Box<dyn AgentRuntime>,
    context_builder: ContextBuilder,
    workspace: Workspace,
    checkpoint_manager: CheckpointManager,
    event_tx: mpsc::UnboundedSender<PipelineEvent>,
    cancel_flag: Option<Arc<AtomicBool>>,
}

impl PipelineRunner {
    /// Create a new pipeline runner.
    ///
    /// `event_tx` is required — all progress is communicated via events.
    pub fn new(
        config: PipelineConfig,
        config_yaml: String,
        workspace_root: PathBuf,
        event_tx: mpsc::UnboundedSender<PipelineEvent>,
    ) -> Self {
        let workspace = Workspace::new(workspace_root.clone());
        let checkpoint_manager = CheckpointManager::new(&workspace_root);
        let runtime = Box::new(ClaudeCodeRuntime::new());
        let context_builder = ContextBuilder::new();

        Self {
            config,
            config_yaml,
            runtime,
            context_builder,
            workspace,
            checkpoint_manager,
            event_tx,
            cancel_flag: None,
        }
    }

    /// Override the agent runtime (for testing or alternative runtimes).
    pub fn with_runtime(mut self, runtime: Box<dyn AgentRuntime>) -> Self {
        self.runtime = runtime;
        self
    }

    /// Set a cancellation flag for cooperative cancellation.
    ///
    /// When the flag is set to `true`, the pipeline will stop after the
    /// current stage and kill any running agent process.
    pub fn with_cancel_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel_flag = Some(flag);
        self
    }

    /// Emit an event.
    fn emit(&self, event: PipelineEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Access the pipeline config.
    pub fn config(&self) -> &PipelineConfig {
        &self.config
    }

    /// Run the pipeline.
    pub async fn run(&self, options: &RunOptions) -> CoreResult<Vec<StageResult>> {
        // Ensure workspace exists
        self.workspace.ensure_exists()?;

        let total_stages = self.config.stages.len();

        // Handle resume
        let (mut checkpoint, start_index) = if options.resume {
            match self.checkpoint_manager.load_latest()? {
                Some(cp) => {
                    CheckpointManager::validate_resume(&cp, &self.config_yaml)?;
                    let idx = cp.next_stage_index;
                    self.emit(PipelineEvent::Resuming {
                        run_id: cp.run_id.clone(),
                        start_index: idx,
                        total_stages,
                    });
                    (cp, idx)
                }
                None => {
                    let cp = CheckpointManager::create_new(&self.config_yaml);
                    (cp, 0)
                }
            }
        } else {
            let cp = CheckpointManager::create_new(&self.config_yaml);
            (cp, 0)
        };

        self.emit(PipelineEvent::PipelineStarted {
            pipeline_name: self.config.name.clone(),
            total_stages,
            run_id: checkpoint.run_id.clone(),
        });

        if options.dry_run {
            return Ok(checkpoint.completed_stages);
        }

        // Execute stages
        let pipeline_start = Instant::now();

        for stage_idx in start_index..total_stages {
            // Check for cancellation before starting each stage
            if let Some(ref flag) = self.cancel_flag {
                if flag.load(Ordering::Relaxed) {
                    let stage_id = self.config.stages[stage_idx].id.clone();
                    self.emit(PipelineEvent::PipelineCancelled {
                        stage_index: stage_idx,
                    });
                    return Err(CoreError::Cancelled { stage_id });
                }
            }

            let stage_config = &self.config.stages[stage_idx];

            self.emit(PipelineEvent::StageStarted {
                stage_index: stage_idx,
                stage_id: stage_config.id.clone(),
                stage_name: stage_config.name.clone(),
                model: stage_config.model.clone(),
            });

            let stage_start = Instant::now();

            // 1. Snapshot workspace
            let before_snapshot = self.workspace.snapshot()?;

            // 2. Build context
            self.emit(PipelineEvent::ContextBuilding {
                stage_index: stage_idx,
            });
            let prior_result = checkpoint.completed_stages.last();
            let context = self
                .context_builder
                .build(&self.config, stage_config, prior_result, &self.workspace)
                .await?;

            self.emit(PipelineEvent::AgentRunning {
                stage_index: stage_idx,
                stage_id: stage_config.id.clone(),
            });

            // 3. Set up tick task for elapsed time tracking
            let tick_tx = self.event_tx.clone();
            let tick_idx = stage_idx;
            let tick_start = stage_start;
            let tick_handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
                loop {
                    interval.tick().await;
                    if tick_tx
                        .send(PipelineEvent::AgentTick {
                            stage_index: tick_idx,
                            elapsed: tick_start.elapsed(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });

            // 4. Create a progress channel that bridges agent activity to PipelineEvent
            let (ptx, mut prx) = mpsc::unbounded_channel::<String>();
            let activity_tx = self.event_tx.clone();
            let activity_idx = stage_idx;
            tokio::spawn(async move {
                while let Some(activity) = prx.recv().await {
                    if activity_tx
                        .send(PipelineEvent::AgentActivity {
                            stage_index: activity_idx,
                            activity,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });

            // 5. Execute agent with timeout
            let timeout_duration = std::time::Duration::from_secs(stage_config.timeout_secs);
            let agent_future = self.runtime.execute(
                stage_config,
                &context,
                self.workspace.root(),
                Some(ptx),
                self.cancel_flag.clone(),
            );

            let agent_result = match tokio::time::timeout(timeout_duration, agent_future).await {
                Ok(result) => result,
                Err(_) => Err(CoreError::AgentError {
                    message: format!(
                        "Stage '{}' timed out after {}s",
                        stage_config.id, stage_config.timeout_secs
                    ),
                }),
            };

            // Stop tick task
            tick_handle.abort();

            let agent_output = match agent_result {
                Ok(output) => output,
                Err(CoreError::Cancelled { ref stage_id }) => {
                    // Save checkpoint so user can resume later
                    checkpoint.next_stage_index = stage_idx;
                    checkpoint.updated_at = Utc::now();
                    let _ = self.checkpoint_manager.save(&checkpoint);
                    self.emit(PipelineEvent::PipelineCancelled {
                        stage_index: stage_idx,
                    });
                    return Err(CoreError::Cancelled {
                        stage_id: stage_id.clone(),
                    });
                }
                Err(e) => {
                    // Save checkpoint before failing so user can resume
                    checkpoint.next_stage_index = stage_idx;
                    checkpoint.updated_at = Utc::now();
                    let checkpoint_saved = self.checkpoint_manager.save(&checkpoint).is_ok();
                    self.emit(PipelineEvent::StageFailed {
                        stage_index: stage_idx,
                        stage_id: stage_config.id.clone(),
                        error: e.to_string(),
                        checkpoint_saved,
                    });
                    self.emit(PipelineEvent::PipelineFailed {
                        error: e.to_string(),
                    });
                    return Err(e);
                }
            };

            // 6. Diff workspace
            let after_snapshot = self.workspace.snapshot()?;
            let artifacts = Workspace::diff(&before_snapshot, &after_snapshot);

            let duration = stage_start.elapsed();

            self.emit(PipelineEvent::StageCompleted {
                stage_index: stage_idx,
                stage_id: stage_config.id.clone(),
                duration,
                artifacts: artifacts.clone(),
                token_usage: agent_output.token_usage.clone(),
            });

            // 7. Build stage result
            let stage_result = StageResult {
                stage_id: stage_config.id.clone(),
                response_text: agent_output.response_text,
                artifacts,
                summary: None, // Will be populated by context builder for next stage
                duration,
                completed_at: Utc::now(),
                token_usage: agent_output.token_usage,
            };

            checkpoint.completed_stages.push(stage_result);
            checkpoint.next_stage_index = stage_idx + 1;
            checkpoint.updated_at = Utc::now();

            // 8. Save checkpoint
            self.checkpoint_manager.save(&checkpoint)?;

            info!(
                stage_id = %stage_config.id,
                duration_secs = duration.as_secs_f64(),
                "Stage completed"
            );
        }

        let total_duration = pipeline_start.elapsed();

        self.emit(PipelineEvent::PipelineCompleted {
            total_duration,
            stages_completed: checkpoint.completed_stages.len(),
        });

        Ok(checkpoint.completed_stages)
    }
}
