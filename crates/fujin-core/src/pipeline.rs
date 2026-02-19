use crate::agent::AgentRuntime;
use crate::checkpoint::CheckpointManager;
use crate::context::ContextBuilder;
use crate::create_runtime;
use crate::error::{CoreError, CoreResult};
use crate::event::PipelineEvent;
use crate::stage::StageResult;
use crate::workspace::Workspace;
use chrono::Utc;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{info, warn};
use fujin_config::{PipelineConfig, StageConfig};

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
    /// Default runtime for the pipeline (from config `runtime` field).
    runtime: Box<dyn AgentRuntime>,
    /// Additional runtimes for per-stage overrides, keyed by runtime name.
    /// Pre-built during construction for all unique stage runtime overrides.
    extra_runtimes: HashMap<String, Box<dyn AgentRuntime>>,
    context_builder: ContextBuilder,
    workspace: Workspace,
    checkpoint_manager: CheckpointManager,
    event_tx: mpsc::UnboundedSender<PipelineEvent>,
    cancel_flag: Option<Arc<AtomicBool>>,
}

impl PipelineRunner {
    /// Create a new pipeline runner.
    ///
    /// Uses the `runtime` field from the pipeline config to select the default
    /// agent runtime. Falls back to `claude-code` if the runtime is unknown.
    /// Pre-builds any additional runtimes needed for per-stage overrides.
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
        let default_runtime_name = config.runtime.clone();
        let runtime = create_runtime(&default_runtime_name).unwrap_or_else(|e| {
            warn!(
                runtime = %default_runtime_name,
                error = %e,
                "Unknown pipeline runtime, falling back to claude-code"
            );
            create_runtime("claude-code").unwrap()
        });

        // Pre-build any extra runtimes needed for per-stage overrides
        let mut extra_runtimes: HashMap<String, Box<dyn AgentRuntime>> = HashMap::new();
        for stage in &config.stages {
            if let Some(ref rt_name) = stage.runtime {
                if rt_name != &default_runtime_name && !extra_runtimes.contains_key(rt_name) {
                    match create_runtime(rt_name) {
                        Ok(rt) => {
                            extra_runtimes.insert(rt_name.clone(), rt);
                        }
                        Err(e) => {
                            warn!(
                                stage_id = %stage.id,
                                runtime = %rt_name,
                                error = %e,
                                "Unknown stage runtime override, will use pipeline default"
                            );
                        }
                    }
                }
            }
        }

        let context_builder = ContextBuilder::with_runtime(default_runtime_name);

        Self {
            config,
            config_yaml,
            runtime,
            extra_runtimes,
            context_builder,
            workspace,
            checkpoint_manager,
            event_tx,
            cancel_flag: None,
        }
    }

    /// Override the default agent runtime (for testing or alternative runtimes).
    pub fn with_runtime(mut self, runtime: Box<dyn AgentRuntime>) -> Self {
        self.runtime = runtime;
        self
    }

    /// Get the appropriate runtime for a stage.
    ///
    /// If the stage has a `runtime` override that differs from the default,
    /// returns the pre-built override runtime. Otherwise returns the default.
    fn runtime_for_stage(&self, stage_config: &StageConfig) -> &dyn AgentRuntime {
        if let Some(ref runtime_name) = stage_config.runtime {
            if let Some(rt) = self.extra_runtimes.get(runtime_name) {
                return rt.as_ref();
            }
        }
        self.runtime.as_ref()
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

    /// Pre-compute retry group stage ranges.
    ///
    /// Returns a map from group name to `(first_stage_index, last_stage_index)`.
    fn compute_retry_group_ranges(&self) -> HashMap<&str, (usize, usize)> {
        let mut ranges: HashMap<&str, (usize, usize)> = HashMap::new();
        for (i, stage) in self.config.stages.iter().enumerate() {
            if let Some(ref group) = stage.retry_group {
                ranges
                    .entry(group.as_str())
                    .and_modify(|(_, last)| *last = i)
                    .or_insert((i, i));
            }
        }
        ranges
    }

    /// Handle a retry-group failure: increment counter, prompt user if limit
    /// reached, emit events, and trim the checkpoint.
    ///
    /// Returns `Ok(first_stage_index)` if the group should retry, or `Err` if
    /// the pipeline should abort (user declined or channel dropped).
    async fn handle_retry_group_failure(
        &self,
        group_name: &str,
        first_idx: usize,
        stage_idx: usize,
        stage_id: &str,
        error_message: &str,
        max_retries: u32,
        retry_counts: &mut HashMap<String, u32>,
        checkpoint: &mut crate::checkpoint::Checkpoint,
    ) -> CoreResult<usize> {
        let count = retry_counts.entry(group_name.to_string()).or_insert(0);
        *count += 1;

        if *count > max_retries {
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.emit(PipelineEvent::RetryLimitReached {
                group_name: group_name.to_string(),
                total_attempts: *count,
                response_tx: tx,
            });

            match rx.await {
                Ok(true) => {
                    // User granted another batch of retries. Reset to 1 so the
                    // RetryGroupAttempt event below reports a sensible number.
                    *count = 1;
                }
                _ => {
                    checkpoint.next_stage_index = stage_idx;
                    checkpoint.updated_at = Utc::now();
                    let checkpoint_saved = self.checkpoint_manager.save(checkpoint).is_ok();
                    self.emit(PipelineEvent::StageFailed {
                        stage_index: stage_idx,
                        stage_id: stage_id.to_string(),
                        error: error_message.to_string(),
                        checkpoint_saved,
                    });
                    self.emit(PipelineEvent::PipelineFailed {
                        error: error_message.to_string(),
                    });
                    return Err(CoreError::AgentError {
                        message: format!(
                            "Retry group '{}' exhausted after {} attempts: {}",
                            group_name, max_retries, error_message
                        ),
                    });
                }
            }
        }

        let attempt = *count;
        self.emit(PipelineEvent::RetryGroupAttempt {
            group_name: group_name.to_string(),
            attempt,
            max_retries,
            first_stage_index: first_idx,
            failed_stage_id: stage_id.to_string(),
            error: error_message.to_string(),
        });

        while checkpoint.completed_stages.len() > first_idx {
            checkpoint.completed_stages.pop();
        }
        checkpoint.next_stage_index = first_idx;
        checkpoint.updated_at = Utc::now();
        let _ = self.checkpoint_manager.save(checkpoint);

        Ok(first_idx)
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

        // Pre-compute retry group ranges: group_name -> (first_index, last_index)
        let retry_group_ranges = self.compute_retry_group_ranges();

        // Track retry attempts per group
        let mut retry_counts: HashMap<String, u32> = HashMap::new();

        // Store verify agent feedback per retry group for injection into stage context on retry
        let mut verify_feedback: HashMap<String, String> = HashMap::new();

        let mut stage_idx = start_index;

        while stage_idx < total_stages {
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
                model: if stage_config.is_command_stage() {
                    "commands".to_string()
                } else {
                    stage_config.model.clone()
                },
                retry_group: stage_config.retry_group.clone(),
            });

            let stage_start = Instant::now();

            // 1. Execute stage (command or agent)
            let stage_exec_result = if stage_config.is_command_stage() {
                self.run_command_stage(stage_idx, stage_config, &stage_start)
                    .await
            } else {
                // Look up verify feedback for this stage's retry group
                let feedback = stage_config
                    .retry_group
                    .as_ref()
                    .and_then(|g| verify_feedback.get(g))
                    .map(|s| s.as_str());

                self.run_agent_stage(
                    stage_idx,
                    stage_config,
                    &checkpoint,
                    &stage_start,
                    feedback,
                )
                .await
            };

            let (response_text, token_usage) = match stage_exec_result {
                Ok(output) => output,
                Err(CoreError::Cancelled { ref stage_id }) => {
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
                    // Check if this stage belongs to a retry group
                    if let Some(ref group_name) = stage_config.retry_group {
                        if let Some(&(first_idx, _last_idx)) = retry_group_ranges.get(group_name.as_str()) {
                            let group_config = self.config.retry_groups.get(group_name);
                            let max_retries = group_config.map_or(5, |g| g.max_retries);

                            // Store the failure error as feedback so the retrying
                            // agent stages know WHY the previous attempt failed.
                            let error_msg = e.to_string();
                            verify_feedback.insert(
                                group_name.clone(),
                                format!(
                                    "Stage '{}' failed with error:\n{}",
                                    stage_config.id, error_msg
                                ),
                            );

                            match self.handle_retry_group_failure(
                                group_name,
                                first_idx,
                                stage_idx,
                                &stage_config.id,
                                &error_msg,
                                max_retries,
                                &mut retry_counts,
                                &mut checkpoint,
                            ).await {
                                Ok(jump_idx) => {
                                    stage_idx = jump_idx;
                                    continue;
                                }
                                Err(abort_err) => return Err(abort_err),
                            }
                        }
                    }

                    // Not in a retry group — fail normally
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

            // 2. Detect file changes via git (fast, even on large repos)
            let ws_root = self.workspace.root().to_path_buf();
            let artifacts = tokio::task::spawn_blocking(move || {
                Workspace::new(ws_root).git_diff()
            })
                .await
                .unwrap_or_default();

            let duration = stage_start.elapsed();

            self.emit(PipelineEvent::StageCompleted {
                stage_index: stage_idx,
                stage_id: stage_config.id.clone(),
                duration,
                artifacts: artifacts.clone(),
                token_usage: token_usage.clone(),
            });

            // 4. Build stage result
            let stage_result = StageResult {
                stage_id: stage_config.id.clone(),
                response_text,
                artifacts,
                summary: None, // Will be populated by context builder for next stage
                duration,
                completed_at: Utc::now(),
                token_usage,
            };

            checkpoint.completed_stages.push(stage_result);

            // Determine if verification is pending before advancing the checkpoint.
            // If this is the last stage in a retry group with a verify agent, we must
            // NOT advance next_stage_index until verification passes — otherwise a
            // crash during verification would skip it on resume.
            let verify_pending = stage_config.retry_group.as_ref().and_then(|group_name| {
                let &(_first, last) = retry_group_ranges.get(group_name.as_str())?;
                if stage_idx != last { return None; }
                let group_cfg = self.config.retry_groups.get(group_name)?;
                group_cfg.verify.as_ref().map(|_| ())
            }).is_some();

            if verify_pending {
                // Keep next_stage_index pointing at this stage so a crash during
                // verification re-runs the last stage (and then verify) on resume.
                checkpoint.next_stage_index = stage_idx;
            } else {
                checkpoint.next_stage_index = stage_idx + 1;
            }
            checkpoint.updated_at = Utc::now();

            // 5. Save checkpoint
            self.checkpoint_manager.save(&checkpoint)?;

            // Clear retry counter on successful completion of a retry group's last stage
            if let Some(ref group_name) = stage_config.retry_group {
                if let Some(&(first_idx, last_idx)) = retry_group_ranges.get(group_name.as_str()) {
                    if stage_idx == last_idx {
                        // Run verification agent if configured
                        let group_config = self.config.retry_groups.get(group_name);
                        if let Some(verify_cfg) = group_config.and_then(|g| g.verify.as_ref()) {
                            self.emit(PipelineEvent::VerifyRunning {
                                group_name: group_name.clone(),
                                stage_index: stage_idx,
                            });

                            let verdict = self.run_verify_agent(
                                verify_cfg,
                                stage_idx,
                                &checkpoint,
                            ).await;

                            match verdict {
                                Ok(verify_response) => {
                                    let passed = parse_verify_verdict(&verify_response);

                                    if passed {
                                        self.emit(PipelineEvent::VerifyPassed {
                                            group_name: group_name.clone(),
                                            stage_index: stage_idx,
                                        });
                                        verify_feedback.remove(group_name);

                                        // Verification passed — now safe to advance checkpoint
                                        checkpoint.next_stage_index = stage_idx + 1;
                                        checkpoint.updated_at = Utc::now();
                                        self.checkpoint_manager.save(&checkpoint)?;
                                    } else {
                                        self.emit(PipelineEvent::VerifyFailed {
                                            group_name: group_name.clone(),
                                            stage_index: stage_idx,
                                            response: verify_response.clone(),
                                        });

                                        // Store feedback so the first stage gets it on retry
                                        verify_feedback.insert(group_name.clone(), verify_response.clone());

                                        let max_retries = group_config.map_or(5, |g| g.max_retries);
                                        match self.handle_retry_group_failure(
                                            group_name,
                                            first_idx,
                                            stage_idx,
                                            &stage_config.id,
                                            &verify_response,
                                            max_retries,
                                            &mut retry_counts,
                                            &mut checkpoint,
                                        ).await {
                                            Ok(jump_idx) => {
                                                stage_idx = jump_idx;
                                                continue;
                                            }
                                            Err(abort_err) => return Err(abort_err),
                                        }
                                    }
                                }
                                Err(e) => {
                                    // Check if this is a cancellation — propagate properly
                                    if matches!(&e, CoreError::Cancelled { .. }) {
                                        checkpoint.next_stage_index = stage_idx;
                                        checkpoint.updated_at = Utc::now();
                                        let _ = self.checkpoint_manager.save(&checkpoint);
                                        self.emit(PipelineEvent::PipelineCancelled {
                                            stage_index: stage_idx,
                                        });
                                        return Err(e);
                                    }

                                    // Agent execution error — treat as verify failure
                                    let error_msg = e.to_string();
                                    self.emit(PipelineEvent::VerifyFailed {
                                        group_name: group_name.clone(),
                                        stage_index: stage_idx,
                                        response: error_msg.clone(),
                                    });

                                    verify_feedback.insert(group_name.clone(), error_msg.clone());

                                    let max_retries = group_config.map_or(5, |g| g.max_retries);
                                    match self.handle_retry_group_failure(
                                        group_name,
                                        first_idx,
                                        stage_idx,
                                        &stage_config.id,
                                        &error_msg,
                                        max_retries,
                                        &mut retry_counts,
                                        &mut checkpoint,
                                    ).await {
                                        Ok(jump_idx) => {
                                            stage_idx = jump_idx;
                                            continue;
                                        }
                                        Err(abort_err) => return Err(abort_err),
                                    }
                                }
                            }
                        }

                        retry_counts.remove(group_name.as_str());
                    }
                }
            }

            info!(
                stage_id = %stage_config.id,
                duration_secs = duration.as_secs_f64(),
                "Stage completed"
            );

            stage_idx += 1;
        }

        let total_duration = pipeline_start.elapsed();

        self.emit(PipelineEvent::PipelineCompleted {
            total_duration,
            stages_completed: checkpoint.completed_stages.len(),
        });

        Ok(checkpoint.completed_stages)
    }

    /// Execute an agent-based stage.
    ///
    /// Returns `(response_text, token_usage)` on success.
    async fn run_agent_stage(
        &self,
        stage_idx: usize,
        stage_config: &StageConfig,
        checkpoint: &crate::checkpoint::Checkpoint,
        stage_start: &Instant,
        verify_feedback: Option<&str>,
    ) -> CoreResult<(String, Option<crate::stage::TokenUsage>)> {
        // Build context
        self.emit(PipelineEvent::ContextBuilding {
            stage_index: stage_idx,
        });
        let prior_result = checkpoint.completed_stages.last();
        let context = self
            .context_builder
            .build(&self.config, stage_config, prior_result, &self.workspace, verify_feedback)
            .await?;

        self.emit(PipelineEvent::StageContext {
            stage_index: stage_idx,
            rendered_prompt: context.rendered_prompt.clone(),
            prior_summary: context.prior_summary.clone(),
        });

        self.emit(PipelineEvent::AgentRunning {
            stage_index: stage_idx,
            stage_id: stage_config.id.clone(),
        });

        // Set up tick task for elapsed time tracking
        let tick_tx = self.event_tx.clone();
        let tick_idx = stage_idx;
        let tick_start = *stage_start;
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

        // Create a progress channel that bridges agent activity to PipelineEvent
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

        // Execute agent (with optional timeout), using per-stage runtime if configured
        let stage_runtime = self.runtime_for_stage(stage_config);
        let agent_future = stage_runtime.execute(
            stage_config,
            &context,
            self.workspace.root(),
            Some(ptx),
            self.cancel_flag.clone(),
        );

        let agent_result = if let Some(timeout_secs) = stage_config.timeout_secs {
            let timeout_duration = std::time::Duration::from_secs(timeout_secs);
            match tokio::time::timeout(timeout_duration, agent_future).await {
                Ok(result) => result,
                Err(_) => Err(CoreError::AgentError {
                    message: format!(
                        "Stage '{}' timed out after {}s",
                        stage_config.id, timeout_secs
                    ),
                }),
            }
        } else {
            agent_future.await
        };

        tick_handle.abort();

        let agent_output = agent_result?;
        Ok((agent_output.response_text, agent_output.token_usage))
    }

    /// Execute a command-based stage by running CLI commands sequentially.
    ///
    /// Returns `(combined_output, None)` on success (no token usage for commands).
    async fn run_command_stage(
        &self,
        stage_idx: usize,
        stage_config: &StageConfig,
        stage_start: &Instant,
    ) -> CoreResult<(String, Option<crate::stage::TokenUsage>)> {
        let commands = stage_config.commands.as_ref().unwrap();
        let total_commands = commands.len();

        // Set up tick task
        let tick_tx = self.event_tx.clone();
        let tick_idx = stage_idx;
        let tick_start = *stage_start;
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

        // Render command templates with pipeline variables
        let mut vars = self.config.variables.clone();
        vars.insert("stage_id".to_string(), stage_config.id.clone());
        vars.insert("stage_name".to_string(), stage_config.name.clone());

        let mut combined_output = String::new();

        for (cmd_idx, cmd_template) in commands.iter().enumerate() {
            // Check cancellation
            if let Some(ref flag) = self.cancel_flag {
                if flag.load(Ordering::Relaxed) {
                    tick_handle.abort();
                    return Err(CoreError::Cancelled {
                        stage_id: stage_config.id.clone(),
                    });
                }
            }

            // Render template variables in the command
            let cmd = render_command_template(cmd_template, &vars)?;

            self.emit(PipelineEvent::CommandRunning {
                stage_index: stage_idx,
                command_index: cmd_idx,
                command: cmd.clone(),
                total_commands,
            });

            let output = self
                .execute_command(&cmd, stage_idx, &stage_config.id)
                .await?;

            if !combined_output.is_empty() {
                combined_output.push_str("\n\n");
            }
            combined_output.push_str(&format!("$ {cmd}\n{output}"));
        }

        tick_handle.abort();

        Ok((combined_output, None))
    }

    /// Run the verification agent for a retry group.
    ///
    /// Returns the agent's response text. The caller parses it for PASS/FAIL.
    async fn run_verify_agent(
        &self,
        verify_cfg: &fujin_config::VerifyConfig,
        stage_idx: usize,
        checkpoint: &crate::checkpoint::Checkpoint,
    ) -> CoreResult<String> {
        // Build a synthetic StageConfig for the verify agent
        let verify_stage = StageConfig {
            id: format!("__verify_{}", stage_idx),
            name: "Verification".to_string(),
            runtime: None,
            model: verify_cfg.model.clone(),
            system_prompt: verify_cfg.system_prompt.clone(),
            user_prompt: verify_cfg.user_prompt.clone(),
            timeout_secs: verify_cfg.timeout_secs,
            allowed_tools: verify_cfg.allowed_tools.clone(),
            commands: None,
            retry_group: None,
        };

        // Build context (includes prior stage results and changed files)
        let prior_result = checkpoint.completed_stages.last();
        let context = self
            .context_builder
            .build(&self.config, &verify_stage, prior_result, &self.workspace, None)
            .await?;

        // Create a progress channel that bridges verify agent activity to PipelineEvent
        let (ptx, mut prx) = mpsc::unbounded_channel::<String>();
        let activity_tx = self.event_tx.clone();
        let activity_idx = stage_idx;
        tokio::spawn(async move {
            while let Some(activity) = prx.recv().await {
                if activity_tx
                    .send(PipelineEvent::VerifyActivity {
                        stage_index: activity_idx,
                        activity,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        // Execute with optional timeout (verify uses default runtime)
        let stage_runtime = self.runtime_for_stage(&verify_stage);
        let agent_future = stage_runtime.execute(
            &verify_stage,
            &context,
            self.workspace.root(),
            Some(ptx),
            self.cancel_flag.clone(),
        );

        let agent_result = if let Some(timeout_secs) = verify_cfg.timeout_secs {
            let timeout_duration = std::time::Duration::from_secs(timeout_secs);
            match tokio::time::timeout(timeout_duration, agent_future).await {
                Ok(result) => result,
                Err(_) => Err(CoreError::AgentError {
                    message: format!("Verification timed out after {}s", timeout_secs),
                }),
            }
        } else {
            agent_future.await
        };

        Ok(agent_result?.response_text)
    }

    /// Execute a single shell command and return its output.
    async fn execute_command(
        &self,
        command: &str,
        stage_idx: usize,
        stage_id: &str,
    ) -> CoreResult<String> {
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::process::Command;
        use std::process::Stdio;

        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", command]);
            c
        };

        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = Command::new("sh");
            c.args(["-c", command]);
            c
        };

        cmd.current_dir(self.workspace.root())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| CoreError::AgentError {
            message: format!("Failed to spawn command '{command}': {e}"),
        })?;

        // Stream both stdout and stderr lines as CommandOutput events
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let event_tx = self.event_tx.clone();
        let stderr_idx = stage_idx;
        let stderr_handle = tokio::spawn(async move {
            let mut lines = Vec::new();
            if let Some(stream) = stderr {
                let mut reader = BufReader::new(stream).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let _ = event_tx.send(PipelineEvent::CommandOutput {
                        stage_index: stderr_idx,
                        line: line.clone(),
                    });
                    lines.push(line);
                }
            }
            lines
        });

        let mut output_lines = Vec::new();
        if let Some(stream) = stdout {
            let mut reader = BufReader::new(stream).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                self.emit(PipelineEvent::CommandOutput {
                    stage_index: stage_idx,
                    line: line.clone(),
                });
                output_lines.push(line);
            }
        }

        let stderr_lines = stderr_handle.await.unwrap_or_default();

        let status = child.wait().await.map_err(|e| CoreError::AgentError {
            message: format!("Failed to wait for command '{command}': {e}"),
        })?;

        let stdout_text = output_lines.join("\n");

        if !status.success() {
            let stderr_text = stderr_lines.join("\n");
            return Err(CoreError::AgentError {
                message: format!(
                    "Command failed in stage '{}' (exit {}): {}\n{}",
                    stage_id,
                    status.code().unwrap_or(-1),
                    command,
                    if stderr_text.is_empty() { &stdout_text } else { &stderr_text }
                ),
            });
        }

        Ok(stdout_text)
    }
}

/// Render `{{variable}}` placeholders in a command string.
fn render_command_template(
    template: &str,
    vars: &std::collections::HashMap<String, String>,
) -> CoreResult<String> {
    let mut hbs = handlebars::Handlebars::new();
    hbs.register_escape_fn(handlebars::no_escape);
    hbs.register_template_string("cmd", template)
        .map_err(|e| CoreError::TemplateError(format!("Invalid command template: {e}")))?;
    hbs.render("cmd", vars)
        .map_err(|e| CoreError::TemplateError(format!("Command template rendering failed: {e}")))
}

/// Parse a verification agent's response for a PASS or FAIL verdict.
///
/// Scans the response for the last occurrence of "PASS" or "FAIL" (case-insensitive,
/// whole word). Returns `true` if the final verdict is PASS, `false` otherwise
/// (including when neither keyword is found).
fn parse_verify_verdict(response: &str) -> bool {
    let response_upper = response.to_uppercase();
    let bytes = response_upper.as_bytes();

    let is_word_boundary = |pos: usize, len: usize| -> bool {
        let before_ok = pos == 0 || !bytes[pos - 1].is_ascii_alphanumeric();
        let after_ok =
            pos + len >= bytes.len() || !bytes[pos + len].is_ascii_alphanumeric();
        before_ok && after_ok
    };

    // Find last whole-word occurrence of each keyword
    let mut last_pass: Option<usize> = None;
    let mut last_fail: Option<usize> = None;

    for (i, _) in response_upper.match_indices("PASS") {
        if is_word_boundary(i, 4) {
            last_pass = Some(i);
        }
    }
    for (i, _) in response_upper.match_indices("FAIL") {
        if is_word_boundary(i, 4) {
            last_fail = Some(i);
        }
    }

    match (last_pass, last_fail) {
        (Some(p), Some(f)) => p > f,
        (Some(_), None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verdict_pass() {
        assert!(parse_verify_verdict("PASS"));
        assert!(parse_verify_verdict("Everything looks correct. PASS"));
        assert!(parse_verify_verdict("The tests pass. Verdict: PASS"));
    }

    #[test]
    fn test_verdict_fail() {
        assert!(!parse_verify_verdict("FAIL"));
        assert!(!parse_verify_verdict("The code is broken. FAIL"));
        assert!(!parse_verify_verdict("Verdict: FAIL"));
    }

    #[test]
    fn test_verdict_last_wins() {
        // When both appear, last one wins
        assert!(parse_verify_verdict("Initially I thought it would FAIL, but actually PASS"));
        assert!(!parse_verify_verdict("It could PASS but there's a bug so FAIL"));
    }

    #[test]
    fn test_verdict_case_insensitive() {
        assert!(parse_verify_verdict("pass"));
        assert!(!parse_verify_verdict("fail"));
        assert!(parse_verify_verdict("Pass"));
        assert!(!parse_verify_verdict("Fail"));
    }

    #[test]
    fn test_verdict_no_keyword() {
        assert!(!parse_verify_verdict(""));
        assert!(!parse_verify_verdict("The code looks fine"));
    }

    #[test]
    fn test_verdict_ignores_substrings() {
        assert!(!parse_verify_verdict("BYPASS the check"));
        assert!(!parse_verify_verdict("FAILOVER to backup"));
        assert!(!parse_verify_verdict("COMPASS is set"));
        assert!(parse_verify_verdict("BYPASS but PASS"));
        assert!(!parse_verify_verdict("PASSING is not enough"));
        assert!(!parse_verify_verdict("FAILED hard"));
    }
}
