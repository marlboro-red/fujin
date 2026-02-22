use crate::agent::AgentRuntime;
use crate::checkpoint::CheckpointManager;
use crate::context::ContextBuilder;
use crate::create_runtime;
use crate::dag::Dag;
use crate::error::{CoreError, CoreResult};
use crate::event::PipelineEvent;
use crate::paths::exports_dir;
use crate::stage::StageResult;
use crate::workspace::Workspace;
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
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
    runtime: Arc<dyn AgentRuntime>,
    /// Additional runtimes for per-stage overrides, keyed by runtime name.
    /// Pre-built during construction for all unique stage runtime overrides.
    extra_runtimes: HashMap<String, Arc<dyn AgentRuntime>>,
    context_builder: ContextBuilder,
    workspace: Workspace,
    checkpoint_manager: CheckpointManager,
    event_tx: mpsc::UnboundedSender<PipelineEvent>,
    cancel_flag: Option<Arc<AtomicBool>>,
}

/// Input data for a spawned stage execution task.
/// Contains all owned data needed to execute a stage in a separate tokio task.
struct StageExecutionInput {
    stage_idx: usize,
    stage_id: String,
    stage_config: StageConfig,
    runtime: Arc<dyn AgentRuntime>,
    workspace_root: PathBuf,
    event_tx: mpsc::UnboundedSender<PipelineEvent>,
    cancel_flag: Option<Arc<AtomicBool>>,
    /// Pipeline variables merged with currently exported variables.
    /// Used by command stages for template rendering.
    exported_vars: HashMap<String, String>,
    stage_start: Instant,

    // --- Context-building data (used by agent stages, built inside spawned task) ---
    /// Runtime name for the summarizer subprocess (e.g. "claude-code").
    context_builder_runtime: String,
    /// Full pipeline config (needed for template variables and summarizer config).
    pipeline_config: PipelineConfig,
    /// Cloned results from direct parent stages.
    prior_results: Vec<StageResult>,
    /// Verify feedback string (retry groups only).
    verify_feedback: Option<String>,
    /// All completed stage results (for {{stages.<id>.*}} template variables).
    all_completed: Vec<StageResult>,
}

/// Output from a completed spawned stage execution task.
struct StageExecutionOutput {
    stage_idx: usize,
    stage_id: String,
    response_text: String,
    token_usage: Option<crate::stage::TokenUsage>,
    duration: Duration,
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
        let runtime: Arc<dyn AgentRuntime> = Arc::from(create_runtime(&default_runtime_name).unwrap_or_else(|e| {
            warn!(
                runtime = %default_runtime_name,
                error = %e,
                "Unknown pipeline runtime, falling back to claude-code"
            );
            create_runtime("claude-code").unwrap()
        }));

        // Pre-build any extra runtimes needed for per-stage overrides
        let mut extra_runtimes: HashMap<String, Arc<dyn AgentRuntime>> = HashMap::new();
        for stage in &config.stages {
            if let Some(ref rt_name) = stage.runtime {
                if rt_name != &default_runtime_name && !extra_runtimes.contains_key(rt_name) {
                    match create_runtime(rt_name) {
                        Ok(rt) => {
                            extra_runtimes.insert(rt_name.clone(), Arc::from(rt));
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
        self.runtime = Arc::from(runtime);
        self
    }

    /// Get the appropriate runtime for a stage.
    ///
    /// If the stage has a `runtime` override that differs from the default,
    /// returns the pre-built override runtime. Otherwise returns the default.
    fn runtime_for_stage(&self, stage_config: &StageConfig) -> Arc<dyn AgentRuntime> {
        if let Some(ref runtime_name) = stage_config.runtime {
            if let Some(rt) = self.extra_runtimes.get(runtime_name) {
                return Arc::clone(rt);
            }
        }
        Arc::clone(&self.runtime)
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
    #[allow(clippy::too_many_arguments)]
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

    /// Run the verify agent for a retry group's last stage.
    ///
    /// Called when the last stage in a retry group finishes (either executed or
    /// skipped).  When the trigger is a *skipped* stage we only verify if at
    /// least one earlier stage in the group actually completed — otherwise
    /// there is nothing to verify.
    ///
    /// Returns `Some(jump_idx)` when verification fails and a retry is needed,
    /// or `None` when verification passed (or was not applicable).
    #[allow(clippy::too_many_arguments)]
    async fn maybe_run_group_verify(
        &self,
        stage_idx: usize,
        triggered_by_skip: bool,
        retry_group_ranges: &HashMap<&str, (usize, usize)>,
        verify_feedback: &mut HashMap<String, String>,
        retry_counts: &mut HashMap<String, u32>,
        checkpoint: &mut crate::checkpoint::Checkpoint,
    ) -> CoreResult<Option<usize>> {
        let stage_config = &self.config.stages[stage_idx];
        let group_name = match stage_config.retry_group.as_ref() {
            Some(n) => n,
            None => return Ok(None),
        };
        let &(first_idx, last_idx) = match retry_group_ranges.get(group_name.as_str()) {
            Some(r) => r,
            None => return Ok(None),
        };
        if stage_idx != last_idx {
            return Ok(None);
        }

        // When triggered by a skip, only verify if the group did real work.
        if triggered_by_skip {
            let group_has_completed = self.config.stages[first_idx..=last_idx]
                .iter()
                .any(|s| checkpoint.completed_stages.iter().any(|sr| sr.stage_id == s.id));
            if !group_has_completed {
                return Ok(None);
            }
        }

        let group_config = self.config.retry_groups.get(group_name);
        let verify_cfg = match group_config.and_then(|g| g.verify.as_ref()) {
            Some(cfg) => cfg,
            None => {
                // No verify configured — clear retry counter and advance.
                retry_counts.remove(group_name.as_str());
                if triggered_by_skip {
                    checkpoint.next_stage_index = stage_idx + 1;
                    checkpoint.updated_at = Utc::now();
                    self.checkpoint_manager.save(checkpoint)?;
                }
                return Ok(None);
            }
        };

        // Hold checkpoint at this stage for crash-safety during verify.
        if triggered_by_skip {
            checkpoint.next_stage_index = stage_idx;
            checkpoint.updated_at = Utc::now();
            self.checkpoint_manager.save(checkpoint)?;
        }

        self.emit(PipelineEvent::VerifyRunning {
            group_name: group_name.clone(),
            stage_index: stage_idx,
        });

        let verdict = self.run_verify_agent(verify_cfg, stage_idx, checkpoint).await;

        match verdict {
            Ok(verify_response) => {
                let passed = parse_verify_verdict(&verify_response);
                if passed {
                    self.emit(PipelineEvent::VerifyPassed {
                        group_name: group_name.clone(),
                        stage_index: stage_idx,
                    });
                    verify_feedback.remove(group_name);
                    retry_counts.remove(group_name.as_str());
                    checkpoint.next_stage_index = stage_idx + 1;
                    checkpoint.updated_at = Utc::now();
                    self.checkpoint_manager.save(checkpoint)?;
                    Ok(None)
                } else {
                    self.emit(PipelineEvent::VerifyFailed {
                        group_name: group_name.clone(),
                        stage_index: stage_idx,
                        response: verify_response.clone(),
                    });
                    verify_feedback.insert(group_name.clone(), verify_response.clone());
                    let max_retries = group_config.map_or(5, |g| g.max_retries);
                    let jump_idx = self.handle_retry_group_failure(
                        group_name,
                        first_idx,
                        stage_idx,
                        &stage_config.id,
                        &verify_response,
                        max_retries,
                        retry_counts,
                        checkpoint,
                    ).await?;
                    Ok(Some(jump_idx))
                }
            }
            Err(e) => {
                if matches!(&e, CoreError::Cancelled { .. }) {
                    checkpoint.next_stage_index = stage_idx;
                    checkpoint.updated_at = Utc::now();
                    let _ = self.checkpoint_manager.save(checkpoint);
                    self.emit(PipelineEvent::PipelineCancelled {
                        stage_index: stage_idx,
                    });
                    return Err(e);
                }
                let error_msg = e.to_string();
                self.emit(PipelineEvent::VerifyFailed {
                    group_name: group_name.clone(),
                    stage_index: stage_idx,
                    response: error_msg.clone(),
                });
                verify_feedback.insert(group_name.clone(), error_msg.clone());
                let max_retries = group_config.map_or(5, |g| g.max_retries);
                let jump_idx = self.handle_retry_group_failure(
                    group_name,
                    first_idx,
                    stage_idx,
                    &stage_config.id,
                    &error_msg,
                    max_retries,
                    retry_counts,
                    checkpoint,
                ).await?;
                Ok(Some(jump_idx))
            }
        }
    }

    /// Run the pipeline.
    pub async fn run(&self, options: &RunOptions) -> CoreResult<Vec<StageResult>> {
        // Ensure workspace exists
        self.workspace.ensure_exists()?;

        let total_stages = self.config.stages.len();
        let dag = Dag::from_config(&self.config);

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
            runtime: self.config.runtime.clone(),
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

        // Accumulate variables exported by stages via their `exports` config
        let mut exported_vars: HashMap<String, String> = HashMap::new();

        // Use sequential execution for linear DAGs (backward compat) or DAG scheduler for parallel
        if dag.is_linear() {
            self.run_sequential(
                start_index,
                &dag,
                &retry_group_ranges,
                &mut retry_counts,
                &mut verify_feedback,
                &mut exported_vars,
                &mut checkpoint,
            )
            .await?;
        } else {
            self.run_dag(
                &dag,
                &retry_group_ranges,
                &mut retry_counts,
                &mut verify_feedback,
                &mut exported_vars,
                &mut checkpoint,
            )
            .await?;
        }

        let total_duration = pipeline_start.elapsed();

        // Aggregate token usage by model
        let mut by_model: HashMap<String, crate::stage::TokenUsage> = HashMap::new();
        for result in &checkpoint.completed_stages {
            if let Some(ref usage) = result.token_usage {
                let entry = by_model.entry(result.model.clone()).or_default();
                entry.input_tokens += usage.input_tokens;
                entry.output_tokens += usage.output_tokens;
            }
        }
        let mut token_usage_by_model: Vec<(String, crate::stage::TokenUsage)> =
            by_model.into_iter().collect();
        token_usage_by_model.sort_by(|a, b| a.0.cmp(&b.0));

        self.emit(PipelineEvent::PipelineCompleted {
            total_duration,
            stages_completed: checkpoint.completed_stages.len(),
            token_usage_by_model,
        });

        Ok(checkpoint.completed_stages)
    }

    /// Execute stages sequentially (linear DAG or backward-compatible mode).
    ///
    /// This preserves the original execution model for pipelines without
    /// explicit `depends_on` or with a purely linear dependency chain.
    #[allow(clippy::too_many_arguments)]
    async fn run_sequential(
        &self,
        start_index: usize,
        dag: &Dag,
        retry_group_ranges: &HashMap<&str, (usize, usize)>,
        retry_counts: &mut HashMap<String, u32>,
        verify_feedback: &mut HashMap<String, String>,
        exported_vars: &mut HashMap<String, String>,
        checkpoint: &mut crate::checkpoint::Checkpoint,
    ) -> CoreResult<()> {
        let total_stages = self.config.stages.len();
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

            // CHECK 1: on_branch gating — skip if no matching branch is active
            if let Some(ref required_branches) = stage_config.on_branch {
                let branch_active = required_branches.iter().any(|b| {
                    checkpoint.active_branches.values().any(|selected| selected == b)
                });
                if !branch_active {
                    let reason = format!(
                        "branch {:?} not active (active: {:?})",
                        required_branches,
                        checkpoint.active_branches.values().collect::<Vec<_>>()
                    );
                    self.emit(PipelineEvent::StageSkipped {
                        stage_index: stage_idx,
                        stage_id: stage_config.id.clone(),
                        reason,
                    });
                    checkpoint.skipped_stages.push(stage_config.id.clone());
                    checkpoint.next_stage_index = stage_idx + 1;
                    checkpoint.updated_at = Utc::now();
                    let _ = self.checkpoint_manager.save(checkpoint);

                    // If this skipped stage is the last in a retry group,
                    // run verify against the work done by earlier stages.
                    if let Some(jump_idx) = self.maybe_run_group_verify(
                        stage_idx, true, retry_group_ranges,
                        verify_feedback, retry_counts, checkpoint,
                    ).await? {
                        stage_idx = jump_idx;
                        continue;
                    }

                    stage_idx += 1;
                    continue;
                }
            }

            // CHECK 2: when condition — skip if regex doesn't match prior stage output
            if let Some(ref when) = stage_config.when {
                if !evaluate_when_condition(when, &checkpoint.completed_stages) {
                    let reason = format!(
                        "when condition not met (stage '{}' output !~ /{}/ )",
                        when.stage, when.output_matches
                    );
                    self.emit(PipelineEvent::StageSkipped {
                        stage_index: stage_idx,
                        stage_id: stage_config.id.clone(),
                        reason,
                    });
                    checkpoint.skipped_stages.push(stage_config.id.clone());
                    checkpoint.next_stage_index = stage_idx + 1;
                    checkpoint.updated_at = Utc::now();
                    let _ = self.checkpoint_manager.save(checkpoint);

                    // If this skipped stage is the last in a retry group,
                    // run verify against the work done by earlier stages.
                    if let Some(jump_idx) = self.maybe_run_group_verify(
                        stage_idx, true, retry_group_ranges,
                        verify_feedback, retry_counts, checkpoint,
                    ).await? {
                        stage_idx = jump_idx;
                        continue;
                    }

                    stage_idx += 1;
                    continue;
                }
            }

            // Execute the stage and process results
            self.execute_and_process_stage(
                stage_idx,
                dag,
                retry_group_ranges,
                retry_counts,
                verify_feedback,
                exported_vars,
                checkpoint,
            )
            .await?;

            // Check if a retry group restarted (checkpoint was rolled back)
            // If next_stage_index < stage_idx + 1, a retry occurred
            if checkpoint.next_stage_index <= stage_idx {
                stage_idx = checkpoint.next_stage_index;
                continue;
            }

            stage_idx += 1;
        }

        Ok(())
    }

    /// Execute stages using DAG-based scheduling with true parallel execution.
    ///
    /// Independent stages (whose dependencies are satisfied) run concurrently
    /// via `tokio::task::JoinSet`. Stages within the same retry group are
    /// executed sequentially within their group, but groups can run in parallel
    /// with other stages.
    ///
    /// Lifecycle per stage:
    /// 1. **Prepare** (sequential, main task): skip checks, context building, emit StageStarted
    /// 2. **Execute** (parallel, spawned tasks): agent/command execution via JoinSet
    /// 3. **Post-process** (sequential, main task): git diff, events, exports, checkpoint
    #[allow(clippy::too_many_arguments)]
    async fn run_dag(
        &self,
        dag: &Dag,
        retry_group_ranges: &HashMap<&str, (usize, usize)>,
        retry_counts: &mut HashMap<String, u32>,
        verify_feedback: &mut HashMap<String, String>,
        exported_vars: &mut HashMap<String, String>,
        checkpoint: &mut crate::checkpoint::Checkpoint,
    ) -> CoreResult<()> {
        let mut completed: HashSet<String> = checkpoint.completed_ids();
        let mut skipped: HashSet<String> = checkpoint.skipped_stages.iter().cloned().collect();
        let mut in_flight: HashSet<String> = HashSet::new();

        // Track which retry group stages are currently being processed
        // to ensure they run sequentially within the group.
        let mut retry_group_in_flight: HashSet<String> = HashSet::new();

        let mut join_set: tokio::task::JoinSet<
            Result<StageExecutionOutput, (usize, String, CoreError)>,
        > = tokio::task::JoinSet::new();

        loop {
            // Check for cancellation
            if let Some(ref flag) = self.cancel_flag {
                if flag.load(Ordering::Relaxed) {
                    join_set.abort_all();
                    let stage_id = in_flight
                        .iter()
                        .next()
                        .cloned()
                        .unwrap_or_else(|| "unknown".to_string());
                    self.emit(PipelineEvent::PipelineCancelled {
                        stage_index: dag.index_of(&stage_id).unwrap_or(0),
                    });
                    return Err(CoreError::Cancelled { stage_id });
                }
            }

            // Find stages ready to run
            let ready = dag.ready_stages(&completed, &skipped, &in_flight);

            if ready.is_empty() && join_set.is_empty() {
                break; // All stages done
            }

            // Launch ready stages that aren't blocked by retry group constraints
            for stage_id in &ready {
                let stage_idx = dag.index_of(stage_id).unwrap();
                let stage_config = &self.config.stages[stage_idx];

                // If this stage is in a retry group, check if another stage
                // from the same group is already in flight (must be sequential).
                if let Some(ref group) = stage_config.retry_group {
                    if retry_group_in_flight.contains(group.as_str()) {
                        continue; // Wait for the other group stage to finish
                    }
                    retry_group_in_flight.insert(group.clone());
                }

                // Check skip conditions (on_branch, when)
                if self.should_skip_stage(stage_idx, stage_config, checkpoint) {
                    let reason = self.get_skip_reason(stage_idx, stage_config, checkpoint);
                    self.emit(PipelineEvent::StageSkipped {
                        stage_index: stage_idx,
                        stage_id: stage_id.clone(),
                        reason,
                    });
                    checkpoint.skipped_stages.push(stage_id.clone());
                    skipped.insert(stage_id.clone());
                    checkpoint.updated_at = Utc::now();
                    let _ = self.checkpoint_manager.save(checkpoint);

                    // Remove from retry group tracking if applicable
                    if let Some(ref group) = stage_config.retry_group {
                        retry_group_in_flight.remove(group.as_str());

                        if let Some(_jump_idx) = self
                            .maybe_run_group_verify(
                                stage_idx,
                                true,
                                retry_group_ranges,
                                verify_feedback,
                                retry_counts,
                                checkpoint,
                            )
                            .await?
                        {
                            if let Some(&(first, last)) = retry_group_ranges
                                .get(stage_config.retry_group.as_ref().unwrap().as_str())
                            {
                                for idx in first..=last {
                                    let id = &self.config.stages[idx].id;
                                    completed.remove(id);
                                    skipped.remove(id);
                                }
                            }
                        }
                    }
                    continue;
                }

                // === PREPARE PHASE (sequential) ===

                let stage_start = Instant::now();
                let stage_runtime = self.runtime_for_stage(stage_config);

                self.emit(PipelineEvent::StageStarted {
                    stage_index: stage_idx,
                    stage_id: stage_id.clone(),
                    stage_name: stage_config.name.clone(),
                    model: if stage_config.is_command_stage() {
                        "commands".to_string()
                    } else {
                        stage_config.model.clone()
                    },
                    runtime: stage_runtime.name().to_string(),
                    allowed_tools: stage_config.allowed_tools.clone(),
                    retry_group: stage_config.retry_group.clone(),
                });

                // Set up exports file path
                if stage_config.exports.is_some() {
                    let export_path = exports_dir(self.workspace.root())
                        .join(&checkpoint.run_id)
                        .join(format!("{}.json", stage_config.id));
                    if let Some(parent) = export_path.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| {
                            CoreError::WorkspaceError {
                                path: parent.to_path_buf(),
                                message: format!(
                                    "Failed to create exports directory: {e}"
                                ),
                            }
                        })?;
                    }
                    exported_vars.insert(
                        "exports_file".to_string(),
                        export_path.display().to_string(),
                    );
                }

                // === EXECUTE PHASE (parallel, spawned) ===

                // Snapshot data needed for context building inside the spawned task
                let parent_ids = dag.parents(&stage_config.id);
                let prior_results: Vec<StageResult> = checkpoint
                    .completed_stages
                    .iter()
                    .filter(|r| parent_ids.contains(&r.stage_id))
                    .cloned()
                    .collect();

                let feedback = stage_config
                    .retry_group
                    .as_ref()
                    .and_then(|g| verify_feedback.get(g))
                    .cloned();

                let all_completed = checkpoint.completed_stages.clone();

                // Snapshot pipeline variables + exported vars for command template rendering
                let merged_vars = {
                    let mut vars = self.config.variables.clone();
                    for (k, v) in exported_vars.iter() {
                        vars.insert(k.clone(), v.clone());
                    }
                    vars
                };

                let input = StageExecutionInput {
                    stage_idx,
                    stage_id: stage_id.clone(),
                    stage_config: stage_config.clone(),
                    runtime: stage_runtime,
                    workspace_root: self.workspace.root().to_path_buf(),
                    event_tx: self.event_tx.clone(),
                    cancel_flag: self.cancel_flag.clone(),
                    exported_vars: merged_vars,
                    stage_start,
                    context_builder_runtime: self.context_builder.runtime_name.clone(),
                    pipeline_config: self.config.clone(),
                    prior_results,
                    verify_feedback: feedback,
                    all_completed,
                };

                join_set.spawn(execute_stage_task(input));
                in_flight.insert(stage_id.clone());
            }

            if join_set.is_empty() {
                // Nothing spawned and nothing in flight — avoid infinite loop
                break;
            }

            // === POST-PROCESS PHASE (sequential, one completed task at a time) ===

            let join_result = join_set.join_next().await;

            match join_result {
                Some(Ok(Ok(output))) => {
                    // Task completed successfully
                    in_flight.remove(&output.stage_id);
                    let stage_config = &self.config.stages[output.stage_idx];
                    if let Some(ref group) = stage_config.retry_group {
                        retry_group_in_flight.remove(group.as_str());
                    }

                    let completed_stage_id = output.stage_id.clone();
                    let completed_stage_idx = output.stage_idx;

                    match self
                        .post_process_stage(
                            output.stage_idx,
                            &output.stage_id,
                            output.response_text,
                            output.token_usage,
                            output.duration,
                            retry_group_ranges,
                            retry_counts,
                            verify_feedback,
                            exported_vars,
                            checkpoint,
                        )
                        .await
                    {
                        Ok(()) => {
                            // Check if a retry group restart occurred
                            if checkpoint.next_stage_index <= completed_stage_idx {
                                let stage_config =
                                    &self.config.stages[completed_stage_idx];
                                if let Some(ref group_name) = stage_config.retry_group {
                                    if let Some(&(first, last)) =
                                        retry_group_ranges.get(group_name.as_str())
                                    {
                                        for idx in first..=last {
                                            let id = &self.config.stages[idx].id;
                                            completed.remove(id);
                                            skipped.remove(id);
                                            in_flight.remove(id);
                                        }
                                        retry_group_in_flight
                                            .remove(group_name.as_str());
                                    }
                                }
                            } else {
                                completed.insert(completed_stage_id);
                            }
                        }
                        Err(e) => {
                            join_set.abort_all();
                            return Err(e);
                        }
                    }
                }
                Some(Ok(Err((stage_idx, stage_id, error)))) => {
                    // Task completed with an error
                    in_flight.remove(&stage_id);
                    let stage_config = &self.config.stages[stage_idx];
                    if let Some(ref group) = stage_config.retry_group {
                        retry_group_in_flight.remove(group.as_str());
                    }

                    match self
                        .handle_stage_error(
                            stage_idx,
                            &stage_id,
                            error,
                            retry_group_ranges,
                            retry_counts,
                            verify_feedback,
                            checkpoint,
                        )
                        .await
                    {
                        Ok(()) => {
                            // Retry group restart — clean up tracking
                            let stage_config = &self.config.stages[stage_idx];
                            if let Some(ref group_name) = stage_config.retry_group {
                                if let Some(&(first, last)) =
                                    retry_group_ranges.get(group_name.as_str())
                                {
                                    for idx in first..=last {
                                        let id = &self.config.stages[idx].id;
                                        completed.remove(id);
                                        skipped.remove(id);
                                        in_flight.remove(id);
                                    }
                                    retry_group_in_flight.remove(group_name.as_str());
                                }
                            }
                        }
                        Err(e) => {
                            join_set.abort_all();
                            return Err(e);
                        }
                    }
                }
                Some(Err(join_error)) => {
                    // Task panicked (JoinError)
                    join_set.abort_all();
                    return Err(CoreError::AgentError {
                        message: format!("Stage task panicked: {}", join_error),
                    });
                }
                None => {
                    // JoinSet is empty — shouldn't happen since we check above
                    break;
                }
            }
        }

        Ok(())
    }

    /// Check if a stage should be skipped (on_branch or when condition).
    fn should_skip_stage(
        &self,
        _stage_idx: usize,
        stage_config: &StageConfig,
        checkpoint: &crate::checkpoint::Checkpoint,
    ) -> bool {
        // on_branch gating
        if let Some(ref required_branches) = stage_config.on_branch {
            let branch_active = required_branches.iter().any(|b| {
                checkpoint.active_branches.values().any(|selected| selected == b)
            });
            if !branch_active {
                return true;
            }
        }

        // when condition
        if let Some(ref when) = stage_config.when {
            if !evaluate_when_condition(when, &checkpoint.completed_stages) {
                return true;
            }
        }

        false
    }

    /// Get the reason why a stage was skipped.
    fn get_skip_reason(
        &self,
        _stage_idx: usize,
        stage_config: &StageConfig,
        checkpoint: &crate::checkpoint::Checkpoint,
    ) -> String {
        if let Some(ref required_branches) = stage_config.on_branch {
            let branch_active = required_branches.iter().any(|b| {
                checkpoint.active_branches.values().any(|selected| selected == b)
            });
            if !branch_active {
                return format!(
                    "branch {:?} not active (active: {:?})",
                    required_branches,
                    checkpoint.active_branches.values().collect::<Vec<_>>()
                );
            }
        }

        if let Some(ref when) = stage_config.when {
            return format!(
                "when condition not met (stage '{}' output !~ /{}/ )",
                when.stage, when.output_matches
            );
        }

        "unknown".to_string()
    }

    /// Execute a single stage and process all post-execution logic.
    ///
    /// Handles: execution, artifact detection, branch classification, exports,
    /// checkpoint updates, and retry group verification. Used by the sequential
    /// execution path.
    #[allow(clippy::too_many_arguments)]
    async fn execute_and_process_stage(
        &self,
        stage_idx: usize,
        dag: &Dag,
        retry_group_ranges: &HashMap<&str, (usize, usize)>,
        retry_counts: &mut HashMap<String, u32>,
        verify_feedback: &mut HashMap<String, String>,
        exported_vars: &mut HashMap<String, String>,
        checkpoint: &mut crate::checkpoint::Checkpoint,
    ) -> CoreResult<()> {
        let stage_config = &self.config.stages[stage_idx];
        let stage_id = stage_config.id.clone();
        let stage_runtime = self.runtime_for_stage(stage_config);

        self.emit(PipelineEvent::StageStarted {
            stage_index: stage_idx,
            stage_id: stage_id.clone(),
            stage_name: stage_config.name.clone(),
            model: if stage_config.is_command_stage() {
                "commands".to_string()
            } else {
                stage_config.model.clone()
            },
            runtime: stage_runtime.name().to_string(),
            allowed_tools: stage_config.allowed_tools.clone(),
            retry_group: stage_config.retry_group.clone(),
        });

        let stage_start = Instant::now();

        // Compute the exports file path and inject {{exports_file}} for stages with exports
        if stage_config.exports.is_some() {
            let export_path = exports_dir(self.workspace.root())
                .join(&checkpoint.run_id)
                .join(format!("{}.json", stage_id));
            if let Some(parent) = export_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| CoreError::WorkspaceError {
                    path: parent.to_path_buf(),
                    message: format!("Failed to create exports directory: {e}"),
                })?;
            }
            exported_vars.insert(
                "exports_file".to_string(),
                export_path.display().to_string(),
            );
        }

        // Execute stage (command or agent)
        let stage_exec_result = if stage_config.is_command_stage() {
            self.run_command_stage(stage_idx, stage_config, &stage_start, exported_vars)
                .await
        } else {
            let feedback = stage_config
                .retry_group
                .as_ref()
                .and_then(|g| verify_feedback.get(g))
                .map(|s| s.as_str());

            self.run_agent_stage_dag(
                stage_idx,
                stage_config,
                dag,
                checkpoint,
                &stage_start,
                feedback,
                exported_vars,
            )
            .await
        };

        match stage_exec_result {
            Ok((response_text, token_usage)) => {
                let duration = stage_start.elapsed();
                self.post_process_stage(
                    stage_idx,
                    &stage_id,
                    response_text,
                    token_usage,
                    duration,
                    retry_group_ranges,
                    retry_counts,
                    verify_feedback,
                    exported_vars,
                    checkpoint,
                )
                .await
            }
            Err(e) => {
                self.handle_stage_error(
                    stage_idx,
                    &stage_id,
                    e,
                    retry_group_ranges,
                    retry_counts,
                    verify_feedback,
                    checkpoint,
                )
                .await
            }
        }
    }

    /// Post-process a completed stage: git diff, events, branch classification,
    /// exports, checkpoint, and retry group verification.
    ///
    /// Shared between the sequential path (`execute_and_process_stage`) and
    /// the parallel DAG path (`run_dag`).
    #[allow(clippy::too_many_arguments)]
    async fn post_process_stage(
        &self,
        stage_idx: usize,
        stage_id: &str,
        response_text: String,
        token_usage: Option<crate::stage::TokenUsage>,
        duration: Duration,
        retry_group_ranges: &HashMap<&str, (usize, usize)>,
        retry_counts: &mut HashMap<String, u32>,
        verify_feedback: &mut HashMap<String, String>,
        exported_vars: &mut HashMap<String, String>,
        checkpoint: &mut crate::checkpoint::Checkpoint,
    ) -> CoreResult<()> {
        let stage_config = &self.config.stages[stage_idx];

        // Detect file changes via git
        let ws_root = self.workspace.root().to_path_buf();
        let artifacts = tokio::task::spawn_blocking(move || {
            Workspace::new(ws_root).git_diff()
        })
        .await
        .unwrap_or_default();

        self.emit(PipelineEvent::StageCompleted {
            stage_index: stage_idx,
            stage_id: stage_id.to_string(),
            duration,
            artifacts: artifacts.clone(),
            token_usage: token_usage.clone(),
        });

        // Run branch classifier if configured
        if let Some(ref branch_config) = stage_config.branch {
            self.emit(PipelineEvent::BranchEvaluating {
                stage_index: stage_idx,
                stage_id: stage_id.to_string(),
            });
            let selected = self
                .run_branch_classifier(branch_config, &response_text, stage_idx)
                .await?;
            checkpoint
                .active_branches
                .insert(stage_id.to_string(), selected.clone());
            self.emit(PipelineEvent::BranchSelected {
                stage_index: stage_idx,
                stage_id: stage_id.to_string(),
                selected_route: selected,
                available_routes: branch_config.routes.clone(),
            });
        }

        // Build stage result
        let stage_result = StageResult {
            stage_id: stage_id.to_string(),
            model: stage_config.model.clone(),
            response_text,
            artifacts,
            summary: None,
            duration,
            completed_at: Utc::now(),
            token_usage,
        };

        checkpoint.completed_stages.push(stage_result);

        // Process exports
        if let Some(ref exports) = stage_config.exports {
            let export_path = exports_dir(self.workspace.root())
                .join(&checkpoint.run_id)
                .join(format!("{}.json", stage_id));
            match std::fs::read_to_string(&export_path) {
                Ok(contents) => {
                    match serde_json::from_str::<HashMap<String, String>>(&contents) {
                        Ok(vars) => {
                            for key in &exports.keys {
                                if !vars.contains_key(key) {
                                    self.emit(PipelineEvent::ExportsWarning {
                                        stage_index: stage_idx,
                                        stage_id: stage_id.to_string(),
                                        message: format!(
                                            "Expected key '{}' not found in exports",
                                            key
                                        ),
                                    });
                                }
                            }

                            // Only insert keys declared in exports.keys
                            let mut loaded: Vec<(String, String)> = Vec::new();
                            for key in &exports.keys {
                                if let Some(v) = vars.get(key) {
                                    exported_vars.insert(key.clone(), v.clone());
                                    loaded.push((key.clone(), v.clone()));
                                }
                            }

                            // Warn about undeclared keys
                            for k in vars.keys() {
                                if !exports.keys.contains(k) {
                                    self.emit(PipelineEvent::ExportsWarning {
                                        stage_index: stage_idx,
                                        stage_id: stage_id.to_string(),
                                        message: format!(
                                            "Undeclared key '{}' in exports file ignored",
                                            k
                                        ),
                                    });
                                }
                            }

                            self.emit(PipelineEvent::ExportsLoaded {
                                stage_index: stage_idx,
                                stage_id: stage_id.to_string(),
                                variables: loaded,
                            });
                        }
                        Err(e) => {
                            self.emit(PipelineEvent::ExportsWarning {
                                stage_index: stage_idx,
                                stage_id: stage_id.to_string(),
                                message: format!("Failed to parse exports file: {}", e),
                            });
                        }
                    }
                }
                Err(e) => {
                    self.emit(PipelineEvent::ExportsWarning {
                        stage_index: stage_idx,
                        stage_id: stage_id.to_string(),
                        message: format!(
                            "Failed to read exports file {}: {}",
                            export_path.display(),
                            e
                        ),
                    });
                }
            }
        }

        // Checkpoint management
        let verify_pending = stage_config
            .retry_group
            .as_ref()
            .and_then(|group_name| {
                let &(_first, last) = retry_group_ranges.get(group_name.as_str())?;
                if stage_idx != last {
                    return None;
                }
                let group_cfg = self.config.retry_groups.get(group_name)?;
                group_cfg.verify.as_ref().map(|_| ())
            })
            .is_some();

        if verify_pending {
            checkpoint.next_stage_index = stage_idx;
        } else {
            checkpoint.next_stage_index = stage_idx + 1;
        }
        checkpoint.updated_at = Utc::now();
        self.checkpoint_manager.save(checkpoint)?;

        // Run verify for retry group
        if let Some(_jump_idx) = self
            .maybe_run_group_verify(
                stage_idx,
                false,
                retry_group_ranges,
                verify_feedback,
                retry_counts,
                checkpoint,
            )
            .await?
        {
            // Retry: checkpoint.next_stage_index was already set by handle_retry_group_failure
            return Ok(());
        }

        info!(
            stage_id = %stage_id,
            duration_secs = duration.as_secs_f64(),
            "Stage completed"
        );

        Ok(())
    }

    /// Handle a stage execution error: cancellation, retry groups, or normal failure.
    ///
    /// Returns `Ok(())` if a retry group restart was triggered, or `Err` to propagate.
    #[allow(clippy::too_many_arguments)]
    async fn handle_stage_error(
        &self,
        stage_idx: usize,
        stage_id: &str,
        error: CoreError,
        retry_group_ranges: &HashMap<&str, (usize, usize)>,
        retry_counts: &mut HashMap<String, u32>,
        verify_feedback: &mut HashMap<String, String>,
        checkpoint: &mut crate::checkpoint::Checkpoint,
    ) -> CoreResult<()> {
        let stage_config = &self.config.stages[stage_idx];

        // Handle cancellation
        if matches!(&error, CoreError::Cancelled { .. }) {
            checkpoint.next_stage_index = stage_idx;
            checkpoint.updated_at = Utc::now();
            let _ = self.checkpoint_manager.save(checkpoint);
            self.emit(PipelineEvent::PipelineCancelled {
                stage_index: stage_idx,
            });
            return Err(error);
        }

        // Check if this stage belongs to a retry group
        if let Some(ref group_name) = stage_config.retry_group {
            if let Some(&(first_idx, _last_idx)) =
                retry_group_ranges.get(group_name.as_str())
            {
                let group_config = self.config.retry_groups.get(group_name);
                let max_retries = group_config.map_or(5, |g| g.max_retries);

                let error_msg = error.to_string();
                verify_feedback.insert(
                    group_name.clone(),
                    format!("Stage '{}' failed with error:\n{}", stage_id, error_msg),
                );

                match self
                    .handle_retry_group_failure(
                        group_name,
                        first_idx,
                        stage_idx,
                        stage_id,
                        &error_msg,
                        max_retries,
                        retry_counts,
                        checkpoint,
                    )
                    .await
                {
                    Ok(_jump_idx) => {
                        // Signal retry by keeping checkpoint.next_stage_index
                        return Ok(());
                    }
                    Err(abort_err) => return Err(abort_err),
                }
            }
        }

        // Not in a retry group — fail normally
        checkpoint.next_stage_index = stage_idx;
        checkpoint.updated_at = Utc::now();
        let checkpoint_saved = self.checkpoint_manager.save(checkpoint).is_ok();
        self.emit(PipelineEvent::StageFailed {
            stage_index: stage_idx,
            stage_id: stage_id.to_string(),
            error: error.to_string(),
            checkpoint_saved,
        });
        self.emit(PipelineEvent::PipelineFailed {
            error: error.to_string(),
        });
        Err(error)
    }

    /// Execute an agent-based stage with DAG-aware context building.
    ///
    /// Uses the stage's DAG parents (direct dependencies) for prior context
    /// instead of just the last completed stage.
    #[allow(clippy::too_many_arguments)]
    async fn run_agent_stage_dag(
        &self,
        stage_idx: usize,
        stage_config: &StageConfig,
        dag: &Dag,
        checkpoint: &crate::checkpoint::Checkpoint,
        stage_start: &Instant,
        verify_feedback: Option<&str>,
        exported_vars: &HashMap<String, String>,
    ) -> CoreResult<(String, Option<crate::stage::TokenUsage>)> {
        // Build context using DAG parents
        self.emit(PipelineEvent::ContextBuilding {
            stage_index: stage_idx,
        });

        let parent_ids = dag.parents(&stage_config.id);
        let prior_results: Vec<&crate::stage::StageResult> = checkpoint
            .completed_stages
            .iter()
            .filter(|r| parent_ids.contains(&r.stage_id))
            .collect();

        let context = self
            .context_builder
            .build(
                &self.config,
                stage_config,
                &prior_results,
                &self.workspace,
                verify_feedback,
                &checkpoint.completed_stages,
                exported_vars,
            )
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

        // Create a progress channel
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

        // Execute agent
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
        exported_vars: &HashMap<String, String>,
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

        // Render command templates with pipeline variables + exported variables
        let mut vars = self.config.variables.clone();
        for (k, v) in exported_vars {
            vars.insert(k.clone(), v.clone());
        }
        vars.insert("stage_id".to_string(), stage_config.id.clone());
        vars.insert("stage_name".to_string(), stage_config.name.clone());

        let mut combined_output = String::new();

        let cmd_future = async {
            for (cmd_idx, cmd_template) in commands.iter().enumerate() {
                // Check cancellation
                if let Some(ref flag) = self.cancel_flag {
                    if flag.load(Ordering::Relaxed) {
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
            Ok(())
        };

        let result: CoreResult<()> = if let Some(timeout_secs) = stage_config.timeout_secs {
            match tokio::time::timeout(Duration::from_secs(timeout_secs), cmd_future).await {
                Ok(r) => r,
                Err(_) => Err(CoreError::AgentError {
                    message: format!(
                        "Stage '{}' timed out after {}s",
                        stage_config.id, timeout_secs
                    ),
                }),
            }
        } else {
            cmd_future.await
        };

        // Always abort the tick task, whether commands succeeded or failed
        tick_handle.abort();

        result?;
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
            when: None,
            branch: None,
            on_branch: None,
            exports: None,
            depends_on: None,
        };

        // Build context (includes prior stage results and changed files)
        // Verify agents don't need exported vars — pass an empty map
        let empty_vars = HashMap::new();
        let prior_results: Vec<&crate::stage::StageResult> = checkpoint
            .completed_stages
            .last()
            .into_iter()
            .collect();
        let context = self
            .context_builder
            .build(&self.config, &verify_stage, &prior_results, &self.workspace, None, &checkpoint.completed_stages, &empty_vars)
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

    /// Run the branch classifier for a stage.
    ///
    /// Builds a classifier prompt from the branch config and stage output,
    /// executes it via the runtime, and parses the response for a route name.
    async fn run_branch_classifier(
        &self,
        branch_config: &fujin_config::BranchConfig,
        stage_output: &str,
        stage_idx: usize,
    ) -> CoreResult<String> {
        let routes_list = branch_config.routes.join(", ");
        let classifier_prompt = format!(
            "{}\n\nStage output:\n---\n{}\n---\n\nYou must respond with exactly one of these routes: {}\n\nRespond with just the route name.",
            branch_config.prompt, stage_output, routes_list
        );

        // Build a synthetic StageConfig for the classifier
        let classifier_stage = StageConfig {
            id: format!("__branch_classifier_{}", stage_idx),
            name: "Branch Classifier".to_string(),
            runtime: None,
            model: branch_config.model.clone(),
            system_prompt: "You are a classifier. Read the stage output and select the most appropriate route. Respond with exactly one route name.".to_string(),
            user_prompt: classifier_prompt.clone(),
            timeout_secs: Some(60),
            allowed_tools: Vec::new(),
            commands: None,
            retry_group: None,
            when: None,
            branch: None,
            on_branch: None,
            exports: None,
            depends_on: None,
        };

        // Build a minimal context (no prior summary needed)
        let context = crate::context::StageContext {
            rendered_prompt: classifier_prompt,
            prior_summary: None,
            changed_files: Vec::new(),
            verify_feedback: None,
        };

        // Execute via the default runtime
        let agent_result = self.runtime.execute(
            &classifier_stage,
            &context,
            self.workspace.root(),
            None,
            self.cancel_flag.clone(),
        ).await?;

        Ok(parse_branch_route(
            &agent_result.response_text,
            &branch_config.routes,
            branch_config.default.as_deref(),
        ))
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

/// Top-level spawnable function for stage execution.
///
/// Dispatches to agent or command execution based on the stage config.
/// Returns `Ok(StageExecutionOutput)` on success, or `Err((stage_idx, stage_id, error))` on failure.
async fn execute_stage_task(
    input: StageExecutionInput,
) -> Result<StageExecutionOutput, (usize, String, CoreError)> {
    let result = if input.stage_config.is_command_stage() {
        execute_command_stage_spawned(&input).await
    } else {
        execute_agent_stage_spawned(&input).await
    };

    match result {
        Ok((response_text, token_usage)) => Ok(StageExecutionOutput {
            stage_idx: input.stage_idx,
            stage_id: input.stage_id,
            response_text,
            token_usage,
            duration: input.stage_start.elapsed(),
        }),
        Err(e) => Err((input.stage_idx, input.stage_id, e)),
    }
}

/// Execute an agent-based stage in a spawned task.
///
/// Builds context in parallel (inside the spawned task), then runs the agent.
async fn execute_agent_stage_spawned(
    input: &StageExecutionInput,
) -> CoreResult<(String, Option<crate::stage::TokenUsage>)> {
    // Build context inside the spawned task (enables parallel context building)
    let _ = input.event_tx.send(PipelineEvent::ContextBuilding {
        stage_index: input.stage_idx,
    });

    let context_builder = ContextBuilder::with_runtime(input.context_builder_runtime.clone());
    let prior_refs: Vec<&StageResult> = input.prior_results.iter().collect();
    let workspace = Workspace::new(input.workspace_root.clone());

    let context = context_builder
        .build(
            &input.pipeline_config,
            &input.stage_config,
            &prior_refs,
            &workspace,
            input.verify_feedback.as_deref(),
            &input.all_completed,
            &input.exported_vars,
        )
        .await?;

    let _ = input.event_tx.send(PipelineEvent::StageContext {
        stage_index: input.stage_idx,
        rendered_prompt: context.rendered_prompt.clone(),
        prior_summary: context.prior_summary.clone(),
    });

    let _ = input.event_tx.send(PipelineEvent::AgentRunning {
        stage_index: input.stage_idx,
        stage_id: input.stage_id.clone(),
    });

    // Set up tick task for elapsed time tracking
    let tick_tx = input.event_tx.clone();
    let tick_idx = input.stage_idx;
    let tick_start = input.stage_start;
    let tick_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
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

    // Create a progress channel
    let (ptx, mut prx) = mpsc::unbounded_channel::<String>();
    let activity_tx = input.event_tx.clone();
    let activity_idx = input.stage_idx;
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

    // Execute agent
    let agent_future = input.runtime.execute(
        &input.stage_config,
        &context,
        &input.workspace_root,
        Some(ptx),
        input.cancel_flag.clone(),
    );

    let agent_result = if let Some(timeout_secs) = input.stage_config.timeout_secs {
        let timeout_duration = Duration::from_secs(timeout_secs);
        match tokio::time::timeout(timeout_duration, agent_future).await {
            Ok(result) => result,
            Err(_) => Err(CoreError::AgentError {
                message: format!(
                    "Stage '{}' timed out after {}s",
                    input.stage_config.id, timeout_secs
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

/// Execute a command-based stage in a spawned task.
///
/// Runs CLI commands sequentially with tick tracking and template variable rendering.
async fn execute_command_stage_spawned(
    input: &StageExecutionInput,
) -> CoreResult<(String, Option<crate::stage::TokenUsage>)> {
    let commands = input.stage_config.commands.as_ref().unwrap();
    let total_commands = commands.len();

    // Set up tick task
    let tick_tx = input.event_tx.clone();
    let tick_idx = input.stage_idx;
    let tick_start = input.stage_start;
    let tick_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
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

    // Render command templates with pipeline variables + exported variables
    let mut vars = input.exported_vars.clone();
    vars.insert("stage_id".to_string(), input.stage_config.id.clone());
    vars.insert("stage_name".to_string(), input.stage_config.name.clone());

    let mut combined_output = String::new();

    let cmd_future = async {
        for (cmd_idx, cmd_template) in commands.iter().enumerate() {
            // Check cancellation
            if let Some(ref flag) = input.cancel_flag {
                if flag.load(Ordering::Relaxed) {
                    return Err(CoreError::Cancelled {
                        stage_id: input.stage_config.id.clone(),
                    });
                }
            }

            let cmd = render_command_template(cmd_template, &vars)?;

            let _ = input.event_tx.send(PipelineEvent::CommandRunning {
                stage_index: input.stage_idx,
                command_index: cmd_idx,
                command: cmd.clone(),
                total_commands,
            });

            let output = execute_command_standalone(
                &cmd,
                &input.workspace_root,
                input.stage_idx,
                &input.stage_id,
                &input.event_tx,
            )
            .await?;

            if !combined_output.is_empty() {
                combined_output.push_str("\n\n");
            }
            combined_output.push_str(&format!("$ {cmd}\n{output}"));
        }
        Ok(())
    };

    let result: CoreResult<()> = if let Some(timeout_secs) = input.stage_config.timeout_secs {
        match tokio::time::timeout(Duration::from_secs(timeout_secs), cmd_future).await {
            Ok(r) => r,
            Err(_) => Err(CoreError::AgentError {
                message: format!(
                    "Stage '{}' timed out after {}s",
                    input.stage_config.id, timeout_secs
                ),
            }),
        }
    } else {
        cmd_future.await
    };

    tick_handle.abort();

    result?;
    Ok((combined_output, None))
}

/// Execute a single shell command (standalone version for spawned tasks).
async fn execute_command_standalone(
    command: &str,
    workspace_root: &std::path::Path,
    stage_idx: usize,
    stage_id: &str,
    event_tx: &mpsc::UnboundedSender<PipelineEvent>,
) -> CoreResult<String> {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

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

    cmd.current_dir(workspace_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| CoreError::AgentError {
        message: format!("Failed to spawn command '{command}': {e}"),
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stderr_tx = event_tx.clone();
    let stderr_idx = stage_idx;
    let stderr_handle = tokio::spawn(async move {
        let mut lines = Vec::new();
        if let Some(stream) = stderr {
            let mut reader = BufReader::new(stream).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let _ = stderr_tx.send(PipelineEvent::CommandOutput {
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
            let _ = event_tx.send(PipelineEvent::CommandOutput {
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
                if stderr_text.is_empty() {
                    &stdout_text
                } else {
                    &stderr_text
                }
            ),
        });
    }

    Ok(stdout_text)
}

/// Shell-escape a string value for safe interpolation into shell commands.
///
/// On POSIX systems, wraps the value in single quotes and escapes internal
/// single quotes. This prevents command injection when template variables
/// (including agent-exported values) are interpolated into `sh -c` commands.
fn shell_escape(s: &str) -> String {
    // POSIX: wrap in single quotes, escape internal single quotes as '\''
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Render `{{variable}}` placeholders in a command string.
///
/// Variable values are shell-escaped before interpolation to prevent
/// command injection (e.g., from agent-exported variables).
fn render_command_template(
    template: &str,
    vars: &std::collections::HashMap<String, String>,
) -> CoreResult<String> {
    // Shell-escape all variable values to prevent command injection
    let escaped_vars: std::collections::HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| (k.clone(), shell_escape(v)))
        .collect();

    let mut hbs = handlebars::Handlebars::new();
    hbs.register_escape_fn(handlebars::no_escape);
    hbs.register_template_string("cmd", template)
        .map_err(|e| CoreError::TemplateError(format!("Invalid command template: {e}")))?;
    hbs.render("cmd", &escaped_vars)
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

/// Evaluate a `when` condition against completed stage results.
///
/// Returns `true` if the referenced stage's `response_text` matches the
/// `output_matches` regex pattern (case-insensitive).
fn evaluate_when_condition(
    when: &fujin_config::WhenCondition,
    completed_stages: &[crate::stage::StageResult],
) -> bool {
    let Some(referenced) = completed_stages.iter().find(|s| s.stage_id == when.stage) else {
        return false;
    };

    let pattern = format!("(?i){}", when.output_matches);
    match regex::RegexBuilder::new(&pattern)
        .size_limit(1_000_000)
        .build()
    {
        Ok(re) => re.is_match(&referenced.response_text),
        Err(_) => false,
    }
}

/// Parse a branch classifier's response for a route name.
///
/// Scans the response for the last whole-word occurrence of each route name
/// (case-insensitive). Returns the route with the highest position.
/// Falls back to `default` if provided, or the first route as a last resort.
fn parse_branch_route(response: &str, routes: &[String], default: Option<&str>) -> String {
    let response_lower = response.to_lowercase();

    let mut best_route: Option<(&str, usize)> = None;

    for route in routes {
        let route_lower = route.to_lowercase();
        // Find last whole-word occurrence
        for (pos, _) in response_lower.match_indices(&route_lower) {
            let bytes = response_lower.as_bytes();
            let before_ok = pos == 0 || !bytes[pos - 1].is_ascii_alphanumeric();
            let after_pos = pos + route_lower.len();
            let after_ok = after_pos >= bytes.len() || !bytes[after_pos].is_ascii_alphanumeric();
            if before_ok && after_ok
                && best_route.is_none_or(|(_, best_pos)| pos > best_pos)
            {
                best_route = Some((route.as_str(), pos));
            }
        }
    }

    if let Some((route, _)) = best_route {
        return route.to_string();
    }

    // Fallback
    if let Some(d) = default {
        return d.to_string();
    }
    routes.first().cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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

    // --- Branch route parsing tests ---

    #[test]
    fn test_branch_route_exact_match() {
        let routes = vec!["frontend".to_string(), "backend".to_string(), "fullstack".to_string()];
        assert_eq!(parse_branch_route("frontend", &routes, None), "frontend");
        assert_eq!(parse_branch_route("backend", &routes, None), "backend");
    }

    #[test]
    fn test_branch_route_case_insensitive() {
        let routes = vec!["frontend".to_string(), "backend".to_string()];
        assert_eq!(parse_branch_route("FRONTEND", &routes, None), "frontend");
        assert_eq!(parse_branch_route("Frontend", &routes, None), "frontend");
    }

    #[test]
    fn test_branch_route_in_sentence() {
        let routes = vec!["frontend".to_string(), "backend".to_string()];
        assert_eq!(
            parse_branch_route("Based on the analysis, I select: frontend", &routes, None),
            "frontend"
        );
    }

    #[test]
    fn test_branch_route_last_wins() {
        let routes = vec!["frontend".to_string(), "backend".to_string()];
        assert_eq!(
            parse_branch_route("frontend initially, but actually backend", &routes, None),
            "backend"
        );
    }

    #[test]
    fn test_branch_route_fallback_to_default() {
        let routes = vec!["frontend".to_string(), "backend".to_string()];
        assert_eq!(
            parse_branch_route("no matching route here", &routes, Some("frontend")),
            "frontend"
        );
    }

    #[test]
    fn test_branch_route_fallback_to_first() {
        let routes = vec!["frontend".to_string(), "backend".to_string()];
        assert_eq!(
            parse_branch_route("no matching route here", &routes, None),
            "frontend"
        );
    }

    #[test]
    fn test_branch_route_ignores_substrings() {
        let routes = vec!["end".to_string(), "backend".to_string()];
        // "frontend" contains "end" but not as a whole word
        assert_eq!(
            parse_branch_route("frontend stuff", &routes, Some("backend")),
            "backend"
        );
    }

    // --- When condition evaluation tests ---

    #[test]
    fn test_when_condition_match() {
        let stages = vec![crate::stage::StageResult {
            stage_id: "analyze".to_string(),
            model: String::new(),
            response_text: "The result is FRONTEND work".to_string(),
            artifacts: crate::artifact::ArtifactSet::new(),
            summary: None,
            duration: Duration::from_secs(1),
            completed_at: Utc::now(),
            token_usage: None,
        }];
        let when = fujin_config::WhenCondition {
            stage: "analyze".to_string(),
            output_matches: "FRONTEND|FULLSTACK".to_string(),
        };
        assert!(evaluate_when_condition(&when, &stages));
    }

    #[test]
    fn test_when_condition_no_match() {
        let stages = vec![crate::stage::StageResult {
            stage_id: "analyze".to_string(),
            model: String::new(),
            response_text: "The result is BACKEND work".to_string(),
            artifacts: crate::artifact::ArtifactSet::new(),
            summary: None,
            duration: Duration::from_secs(1),
            completed_at: Utc::now(),
            token_usage: None,
        }];
        let when = fujin_config::WhenCondition {
            stage: "analyze".to_string(),
            output_matches: "FRONTEND|FULLSTACK".to_string(),
        };
        assert!(!evaluate_when_condition(&when, &stages));
    }

    #[test]
    fn test_when_condition_missing_stage() {
        let stages = vec![];
        let when = fujin_config::WhenCondition {
            stage: "nonexistent".to_string(),
            output_matches: ".*".to_string(),
        };
        assert!(!evaluate_when_condition(&when, &stages));
    }

    #[test]
    fn test_when_condition_case_insensitive() {
        let stages = vec![crate::stage::StageResult {
            stage_id: "s1".to_string(),
            model: String::new(),
            response_text: "frontend".to_string(),
            artifacts: crate::artifact::ArtifactSet::new(),
            summary: None,
            duration: Duration::from_secs(1),
            completed_at: Utc::now(),
            token_usage: None,
        }];
        let when = fujin_config::WhenCondition {
            stage: "s1".to_string(),
            output_matches: "FRONTEND".to_string(),
        };
        assert!(evaluate_when_condition(&when, &stages));
    }
}
