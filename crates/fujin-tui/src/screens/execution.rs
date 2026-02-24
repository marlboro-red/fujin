use crate::theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fujin_core::event::PipelineEvent;
use fujin_core::artifact::ArtifactSet;
use fujin_core::stage::TokenUsage;
use fujin_core::util::truncate_chars;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, LineGauge, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
    Frame,
};
use std::time::{Duration, Instant};

/// Status of a single stage in the execution view.
#[derive(Debug, Clone)]
pub enum StageStatus {
    Pending,
    Building,
    Running {
        elapsed: Duration,
        activity_log: Vec<String>,
    },
    Completed {
        duration: Duration,
        artifacts: ArtifactSet,
        token_usage: Option<TokenUsage>,
        activity_log: Vec<String>,
    },
    Failed { error: String },
    Skipped { reason: String },
}

/// What the user has selected in the stage list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionTarget {
    /// A pipeline stage (by index).
    Stage(usize),
    /// A retry group's verification step (by group name).
    Verify(String),
}
#[derive(Debug, Clone)]
pub enum VerifyStatus {
    /// Waiting (group stages still running, or verify not yet triggered).
    Pending,
    /// Verification agent is running.
    Running { activity_log: Vec<String> },
    /// Verification passed.
    Passed,
    /// Verification failed.
    Failed { response: String },
}

/// Info for a stage displayed in the execution view.
#[derive(Debug, Clone)]
pub struct StageInfo {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub model: String,
    /// Runtime name (e.g. "claude-code", "copilot-cli").
    pub runtime: String,
    /// Allowed tools for this stage.
    pub allowed_tools: Vec<String>,
    pub status: StageStatus,
    pub rendered_prompt: Option<String>,
    pub prior_summary: Option<String>,
    /// Whether a retry-group verification is currently running for this stage.
    pub verifying: bool,
    /// Retry group this stage belongs to (if any).
    pub retry_group: Option<String>,
    /// Parallel group this stage belongs to (if any).
    pub parallel_group: Option<usize>,
    /// Branch selection info (if this stage ran a branch classifier).
    pub branch_info: Option<(String, Vec<String>)>,
    /// Whether the stage is running in an isolated git worktree.
    pub isolated: bool,
    /// Exported variables from this stage (populated on ExportsLoaded).
    pub exported_vars: Vec<(String, String)>,
}

/// State for the pipeline execution screen.
pub struct ExecutionState {
    /// Pipeline name.
    pub pipeline_name: String,
    /// Run ID.
    pub run_id: String,
    /// Total stage count.
    pub total_stages: usize,
    /// Info about each stage.
    pub stages: Vec<StageInfo>,
    /// Currently active stage index.
    pub active_stage: Option<usize>,
    /// Which item is selected for detail viewing.
    pub selection: SelectionTarget,
    /// Pipeline start time.
    pub start_time: Instant,
    /// Frozen elapsed time (set when pipeline finishes).
    pub final_elapsed: Option<Duration>,
    /// Cumulative token usage.
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    /// Per-model token breakdown (populated on pipeline completion).
    pub token_usage_by_model: Vec<(String, TokenUsage)>,
    /// Whether the pipeline has finished (success or failure).
    pub finished: bool,
    /// Final status message.
    pub final_message: Option<String>,
    /// Spinner frame index for the running animation.
    pub spinner_frame: usize,
    /// Manual scroll offset for the detail/log panel.
    pub detail_scroll: u16,
    /// Whether the user has manually scrolled (disables auto-scroll).
    pub detail_scroll_pinned: bool,
    /// Total rendered line count of the detail panel (for scroll bounds).
    detail_content_height: usize,
    /// Whether the detail panel is showing the prompt/context view.
    pub show_prompt: bool,
    /// Whether the detail panel is focused (maximized).
    pub detail_focused: bool,
    /// Current retry attempt per group name.
    pub retry_attempts: std::collections::HashMap<String, (u32, u32)>,
    /// Verification status per retry group name.
    pub verify_states: std::collections::HashMap<String, VerifyStatus>,
    /// Whether the user has pressed q once (confirm cancel).
    pub confirm_cancel: bool,
}

/// Actions the execution screen can request.
pub enum ExecutionAction {
    None,
    BackToBrowser,
    Quit,
    ToggleHelp,
    CancelPipeline,
}

const SPINNER_FRAMES: &[&str] = &["\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}", "\u{2827}", "\u{2807}", "\u{280f}"];

impl ExecutionState {
    pub fn new(
        pipeline_name: String,
        run_id: String,
        total_stages: usize,
        stage_names: Vec<(String, String, String, String, Option<String>, Option<usize>)>,
    ) -> Self {
        let stages = stage_names
            .into_iter()
            .enumerate()
            .map(|(i, (id, name, model, runtime, retry_group, parallel_group))| StageInfo {
                index: i,
                id,
                name,
                model,
                runtime,
                allowed_tools: Vec::new(),
                status: StageStatus::Pending,
                rendered_prompt: None,
                prior_summary: None,
                verifying: false,
                retry_group,
                parallel_group,
                branch_info: None,
                isolated: false,
                exported_vars: Vec::new(),
            })
            .collect();
        Self {
            pipeline_name,
            run_id,
            total_stages,
            stages,
            active_stage: None,
            selection: SelectionTarget::Stage(0),
            start_time: Instant::now(),
            final_elapsed: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            token_usage_by_model: Vec::new(),
            finished: false,
            final_message: None,
            spinner_frame: 0,
            detail_scroll: 0,
            detail_scroll_pinned: false,
            detail_content_height: 0,
            show_prompt: false,
            detail_focused: false,
            retry_attempts: std::collections::HashMap::new(),
            verify_states: std::collections::HashMap::new(),
            confirm_cancel: false,
        }
    }

    /// Count how many stages are completed (including skipped).
    fn completed_stages(&self) -> usize {
        self.stages
            .iter()
            .filter(|s| matches!(s.status, StageStatus::Completed { .. } | StageStatus::Skipped { .. }))
            .count()
    }

    /// Build the ordered flat list of selectable items (stages + verify entries).
    fn selectable_items(&self) -> Vec<SelectionTarget> {
        let mut items = Vec::new();
        let mut seen_groups: std::collections::HashSet<&str> = std::collections::HashSet::new();

        // Pre-compute last stage index per group
        let mut group_last: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for (i, stage) in self.stages.iter().enumerate() {
            if let Some(ref g) = stage.retry_group {
                group_last.insert(g.as_str(), i);
            }
        }

        for (i, stage) in self.stages.iter().enumerate() {
            items.push(SelectionTarget::Stage(i));
            // After the last stage of a retry group, insert a Verify entry
            if let Some(ref g) = stage.retry_group {
                if group_last.get(g.as_str()) == Some(&i) && !seen_groups.contains(g.as_str()) {
                    seen_groups.insert(g.as_str());
                    items.push(SelectionTarget::Verify(g.clone()));
                }
            }
        }
        items
    }

    /// Navigate selection by delta (-1 = up, +1 = down).
    fn move_selection(&mut self, delta: i32) {
        let items = self.selectable_items();
        if items.is_empty() {
            return;
        }
        let cur_pos = items.iter().position(|t| *t == self.selection).unwrap_or(0);
        let new_pos = if delta < 0 {
            cur_pos.saturating_sub((-delta) as usize)
        } else {
            (cur_pos + delta as usize).min(items.len() - 1)
        };
        self.selection = items[new_pos].clone();
    }

    /// Jump selection to first item.
    fn jump_to_first(&mut self) {
        let items = self.selectable_items();
        if let Some(first) = items.into_iter().next() {
            self.selection = first;
        }
    }

    /// Jump selection to last item.
    fn jump_to_last(&mut self) {
        let items = self.selectable_items();
        if let Some(last) = items.into_iter().last() {
            self.selection = last;
        }
    }

    /// Handle an incoming pipeline event.
    pub fn handle_pipeline_event(&mut self, event: PipelineEvent) {
        match event {
            PipelineEvent::PipelineStarted { .. } | PipelineEvent::Resuming { .. } => {}

            PipelineEvent::StageStarted {
                stage_index,
                stage_id,
                stage_name,
                model,
                runtime,
                allowed_tools,
                retry_group,
                isolated,
            } => {
                // Ensure stages vec is large enough
                while self.stages.len() <= stage_index {
                    self.stages.push(StageInfo {
                        index: self.stages.len(),
                        id: String::new(),
                        name: String::new(),
                        model: String::new(),
                        runtime: String::new(),
                        allowed_tools: Vec::new(),
                        status: StageStatus::Pending,
                        rendered_prompt: None,
                        prior_summary: None,
                        verifying: false,
                        retry_group: None,
                        parallel_group: None,
                        branch_info: None,
                        isolated: false,
                        exported_vars: Vec::new(),
                    });
                }
                let stage = &mut self.stages[stage_index];
                stage.id = stage_id;
                stage.name = stage_name;
                stage.runtime = runtime;
                stage.allowed_tools = allowed_tools;
                stage.model = model.clone();
                stage.retry_group = retry_group;
                stage.isolated = isolated;
                // Immediately transition to Running so the TUI shows progress
                stage.status = StageStatus::Running {
                    elapsed: Duration::ZERO,
                    activity_log: if model == "commands" {
                        vec!["Starting commands...".to_string()]
                    } else {
                        Vec::new()
                    },
                };
                self.active_stage = Some(stage_index);
                self.selection = SelectionTarget::Stage(stage_index);
            }

            PipelineEvent::ContextBuilding { stage_index } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    stage.status = StageStatus::Building;
                }
            }

            PipelineEvent::StageContext {
                stage_index,
                rendered_prompt,
                prior_summary,
            } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    stage.rendered_prompt = Some(rendered_prompt);
                    stage.prior_summary = prior_summary;
                }
            }

            PipelineEvent::AgentRunning { stage_index, .. } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    stage.status = StageStatus::Running {
                        elapsed: Duration::ZERO,
                        activity_log: Vec::new(),
                    };
                }
            }

            PipelineEvent::CommandRunning {
                stage_index,
                command_index,
                command,
                total_commands,
            } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    let activity = format!(
                        "$ {} [{}/{}]",
                        command,
                        command_index + 1,
                        total_commands,
                    );
                    match &mut stage.status {
                        StageStatus::Running { activity_log, .. } => {
                            activity_log.push(activity);
                        }
                        _ => {
                            stage.status = StageStatus::Running {
                                elapsed: Duration::ZERO,
                                activity_log: vec![activity],
                            };
                        }
                    }
                }
            }

            PipelineEvent::CommandOutput {
                stage_index,
                line,
            } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    if let StageStatus::Running { activity_log, .. } = &mut stage.status {
                        activity_log.push(line);
                    }
                }
            }

            PipelineEvent::AgentTick {
                stage_index,
                elapsed,
            } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    if let StageStatus::Running { activity_log, .. } = &mut stage.status {
                        let log = std::mem::take(activity_log);
                        stage.status = StageStatus::Running {
                            elapsed,
                            activity_log: log,
                        };
                    } else {
                        stage.status = StageStatus::Running {
                            elapsed,
                            activity_log: Vec::new(),
                        };
                    }
                }
                self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
            }

            PipelineEvent::AgentActivity {
                stage_index,
                activity,
            } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    if let StageStatus::Running { activity_log, .. } = &mut stage.status {
                        activity_log.push(activity);
                    }
                }
            }

            PipelineEvent::StageCompleted {
                stage_index,
                duration,
                artifacts,
                token_usage,
                ..
            } => {
                if let Some(ref usage) = token_usage {
                    self.total_input_tokens += usage.input_tokens;
                    self.total_output_tokens += usage.output_tokens;
                }
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    // Preserve activity log from running state
                    let log = if let StageStatus::Running { activity_log, .. } = &mut stage.status
                    {
                        std::mem::take(activity_log)
                    } else {
                        Vec::new()
                    };
                    stage.status = StageStatus::Completed {
                        duration,
                        artifacts,
                        token_usage,
                        activity_log: log,
                    };
                }
                self.active_stage = None;
            }

            PipelineEvent::StageFailed {
                stage_index,
                error,
                ..
            } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    stage.status = StageStatus::Failed {
                        error: error.clone(),
                    };
                }
                self.active_stage = None;
            }

            PipelineEvent::PipelineCompleted {
                total_duration,
                stages_completed,
                token_usage_by_model,
            } => {
                self.finished = true;
                self.final_elapsed = Some(self.start_time.elapsed());
                self.token_usage_by_model = token_usage_by_model;
                self.final_message = Some(format!(
                    "Pipeline complete in {:.1}s \u{2014} {} stages executed",
                    total_duration.as_secs_f64(),
                    stages_completed
                ));
            }

            PipelineEvent::PipelineFailed { error } => {
                self.finished = true;
                self.final_elapsed = Some(self.start_time.elapsed());
                self.final_message = Some(format!("Pipeline failed: {error}"));
            }

            PipelineEvent::PipelineCancelled { stage_index } => {
                // Mark the current running stage as failed with cancel message
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    stage.status = StageStatus::Failed {
                        error: "Cancelled by user".to_string(),
                    };
                }
                self.active_stage = None;
                self.finished = true;
                self.final_elapsed = Some(self.start_time.elapsed());
                self.final_message =
                    Some(format!("Pipeline cancelled at stage {}", stage_index + 1));
            }

            PipelineEvent::RetryGroupAttempt {
                group_name,
                attempt,
                max_retries,
                first_stage_index,
                failed_stage_id,
                error,
            } => {
                // Mark the failed stage, then reset stages in the retry group
                if let Some(stage) = self.stages.iter_mut().find(|s| s.id == failed_stage_id) {
                    stage.status = StageStatus::Failed {
                        error: format!(
                            "Retrying ({}/{}) \u{2014} {}",
                            attempt, max_retries, error
                        ),
                    };
                }
                // Reset stages from first_stage_index onwards back to Pending
                for stage in self.stages.iter_mut().skip(first_stage_index) {
                    if stage.id != failed_stage_id {
                        stage.status = StageStatus::Pending;
                    }
                }
                self.retry_attempts.insert(group_name.clone(), (attempt, max_retries));
                self.verify_states.insert(group_name, VerifyStatus::Pending);
                self.active_stage = None;
            }

            PipelineEvent::RetryLimitReached {
                response_tx,
                ..
            } => {
                // In TUI mode, auto-continue (the TUI doesn't have a blocking prompt)
                let _ = response_tx.send(true);
            }

            PipelineEvent::VerifyRunning { group_name, stage_index } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    stage.verifying = true;
                }
                self.verify_states.insert(
                    group_name,
                    VerifyStatus::Running { activity_log: Vec::new() },
                );
            }

            PipelineEvent::VerifyPassed { group_name, stage_index } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    stage.verifying = false;
                }
                self.verify_states.insert(group_name, VerifyStatus::Passed);
            }

            PipelineEvent::VerifyFailed { group_name, stage_index, response } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    stage.verifying = false;
                }
                self.verify_states.insert(
                    group_name,
                    VerifyStatus::Failed { response },
                );
            }

            PipelineEvent::VerifyActivity { stage_index: _, activity } => {
                // Find the verify state that's currently running and append activity
                for state in self.verify_states.values_mut() {
                    if let VerifyStatus::Running { activity_log } = state {
                        activity_log.push(activity);
                        break;
                    }
                }
            }

            PipelineEvent::StageSkipped {
                stage_index,
                stage_id: _,
                reason,
            } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    stage.status = StageStatus::Skipped { reason };
                }
            }

            PipelineEvent::BranchEvaluating { .. } => {
                // No visual change needed; the stage is already shown as completed
            }

            PipelineEvent::BranchSelected {
                stage_index,
                selected_route,
                available_routes,
                ..
            } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    stage.branch_info = Some((selected_route, available_routes));
                }
            }

            PipelineEvent::ExportsLoaded { stage_index, variables, .. } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    if let StageStatus::Completed { activity_log, .. } = &mut stage.status {
                        let names: Vec<&str> = variables.iter().map(|(k, _)| k.as_str()).collect();
                        activity_log.push(format!("Exported variables: {}", names.join(", ")));
                    }
                    stage.exported_vars = variables;
                }
            }

            PipelineEvent::ExportsWarning { stage_index, message, .. } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    if let StageStatus::Completed { activity_log, .. } = &mut stage.status {
                        activity_log.push(format!("Export warning: {}", message));
                    }
                }
            }
        }
    }

    /// Handle a key event. Returns the resulting action.
    pub fn handle_key(&mut self, key: KeyEvent) -> ExecutionAction {
        // Any key dismisses the confirm-cancel prompt (except a second q/Esc)
        if self.confirm_cancel {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('y') => {
                    self.confirm_cancel = false;
                    return ExecutionAction::CancelPipeline;
                }
                _ => {
                    // Dismiss the warning
                    self.confirm_cancel = false;
                    return ExecutionAction::None;
                }
            }
        }

        match key.code {
            // Ctrl+C force-cancels a running pipeline
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.finished {
                    ExecutionAction::CancelPipeline
                } else {
                    ExecutionAction::Quit
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.detail_focused {
                    self.detail_focused = false;
                    ExecutionAction::None
                } else if self.finished {
                    ExecutionAction::Quit
                } else {
                    // First press: show confirm warning
                    self.confirm_cancel = true;
                    ExecutionAction::None
                }
            }
            KeyCode::Char('b') if self.finished && !self.detail_focused => {
                ExecutionAction::BackToBrowser
            }
            KeyCode::Char('?') => ExecutionAction::ToggleHelp,
            KeyCode::Char('f') | KeyCode::Enter => {
                self.detail_focused = !self.detail_focused;
                ExecutionAction::None
            }
            KeyCode::Tab => {
                self.detail_focused = !self.detail_focused;
                ExecutionAction::None
            }
            KeyCode::Char('p') => {
                self.show_prompt = !self.show_prompt;
                self.detail_scroll = 0;
                self.detail_scroll_pinned = false;
                ExecutionAction::None
            }
            // Stage navigation
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                self.detail_scroll = 0;
                self.detail_scroll_pinned = false;
                ExecutionAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                self.detail_scroll = 0;
                self.detail_scroll_pinned = false;
                ExecutionAction::None
            }
            KeyCode::Char('G') => {
                self.jump_to_last();
                self.detail_scroll = 0;
                self.detail_scroll_pinned = false;
                ExecutionAction::None
            }
            KeyCode::Char('g') => {
                self.jump_to_first();
                self.detail_scroll = 0;
                self.detail_scroll_pinned = false;
                ExecutionAction::None
            }
            // Log scrolling (always available)
            KeyCode::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(10);
                self.detail_scroll_pinned = true;
                ExecutionAction::None
            }
            KeyCode::PageDown => {
                self.scroll_detail_down(10);
                ExecutionAction::None
            }
            KeyCode::Home => {
                self.detail_scroll = 0;
                self.detail_scroll_pinned = true;
                ExecutionAction::None
            }
            KeyCode::End => {
                self.detail_scroll_pinned = false;
                ExecutionAction::None
            }
            _ => ExecutionAction::None,
        }
    }

    /// Scroll the detail panel down by `delta` lines, clamped to content bounds.
    fn scroll_detail_down(&mut self, delta: u16) {
        let max = self.detail_content_height.saturating_sub(1) as u16;
        self.detail_scroll = (self.detail_scroll + delta).min(max);
        self.detail_scroll_pinned = true;
    }

    /// Compute how many lines the footer needs.
    fn footer_height(&self) -> u16 {
        // 1 line for separator + 1 line for controls/total
        let base: u16 = 2;
        if self.finished && self.token_usage_by_model.len() > 1 {
            // Per-model lines + total line
            base + self.token_usage_by_model.len() as u16 + 1
        } else {
            base
        }
    }

    /// Render the execution screen.
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let footer_h = self.footer_height();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),      // header
                Constraint::Length(1),      // progress bar
                Constraint::Min(0),        // main content
                Constraint::Length(footer_h), // footer
            ])
            .split(area);

        self.render_header(frame, chunks[0]);
        self.render_progress_bar(frame, chunks[1]);
        self.render_body(frame, chunks[2]);
        self.render_footer(frame, chunks[3]);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let elapsed = self.final_elapsed.unwrap_or_else(|| self.start_time.elapsed());
        let elapsed_str = format_duration(elapsed);
        let run_short: String = self.run_id.chars().take(8).collect();

        let (status, status_color) = if self.finished {
            if self.final_message.as_ref().is_some_and(|m| m.contains("failed") || m.contains("cancelled")) {
                ("Failed", theme::ERROR)
            } else {
                ("Finished", theme::SUCCESS)
            }
        } else {
            ("Running", theme::WARNING)
        };

        let header = Line::from(vec![
            Span::styled(
                "  fujin",
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" \u{2502} ", Style::default().fg(theme::BORDER)),
            Span::styled(status, Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
            Span::styled(": ", Style::default().fg(theme::TEXT_SECONDARY)),
            Span::styled(
                &self.pipeline_name,
                Style::default()
                    .fg(theme::TEXT_PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  run: {run_short}  {elapsed_str}"),
                Style::default().fg(theme::TEXT_MUTED),
            ),
        ]);
        let sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);
        frame.render_widget(Paragraph::new(header), sub[0]);
        let sep = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(theme::BORDER));
        frame.render_widget(sep, sub[1]);
    }

    fn render_progress_bar(&self, frame: &mut Frame, area: Rect) {
        let completed = self.completed_stages();
        let total = self.total_stages.max(1);
        let ratio = (completed as f64 / total as f64).min(1.0);

        let color = if self.finished {
            if self.final_message.as_ref().is_some_and(|m| m.contains("failed") || m.contains("cancelled")) {
                theme::ERROR
            } else {
                theme::SUCCESS
            }
        } else {
            theme::ACCENT
        };

        let label = format!(" {completed}/{total} ");
        let gauge = LineGauge::default()
            .ratio(ratio)
            .label(Span::styled(label, Style::default().fg(theme::TEXT_SECONDARY)))
            .filled_style(Style::default().fg(color))
            .unfilled_style(Style::default().fg(theme::BORDER))
            .line_set(ratatui::symbols::line::THICK);

        frame.render_widget(gauge, area);
    }

    fn render_body(&mut self, frame: &mut Frame, area: Rect) {
        if self.detail_focused {
            self.render_stage_detail(frame, area);
        } else {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(area);

            self.render_stage_list(frame, chunks[0]);
            self.render_stage_detail(frame, chunks[1]);
        }
    }

    fn render_stage_list(&self, frame: &mut Frame, area: Rect) {
        let block = theme::styled_block("Stages", false);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Pre-compute retry group ranges: group_name -> (first_idx, last_idx)
        let mut retry_group_ranges: std::collections::HashMap<&str, (usize, usize)> =
            std::collections::HashMap::new();
        for (i, stage) in self.stages.iter().enumerate() {
            if let Some(ref g) = stage.retry_group {
                retry_group_ranges
                    .entry(g.as_str())
                    .and_modify(|(first, last)| {
                        if i < *first { *first = i; }
                        if i > *last { *last = i; }
                    })
                    .or_insert((i, i));
            }
        }

        // Pre-compute parallel group ranges: group_id -> (first_idx, last_idx)
        let mut parallel_group_ranges: std::collections::HashMap<usize, (usize, usize)> =
            std::collections::HashMap::new();
        for (i, stage) in self.stages.iter().enumerate() {
            if let Some(g) = stage.parallel_group {
                parallel_group_ranges
                    .entry(g)
                    .and_modify(|(first, last)| {
                        if i < *first { *first = i; }
                        if i > *last { *last = i; }
                    })
                    .or_insert((i, i));
            }
        }

        // Determine the visual group for each stage: retry takes precedence
        // VisualGroup: None, Some(("retry", group_name)) or Some(("parallel", group_id_str))
        let visual_groups: Vec<Option<(&str, String)>> = self.stages.iter().map(|s| {
            if s.retry_group.is_some() {
                Some(("retry", s.retry_group.as_ref().unwrap().clone()))
            } else if let Some(pg) = s.parallel_group {
                Some(("parallel", pg.to_string()))
            } else {
                None
            }
        }).collect();

        // Compute the width needed for stage numbers
        let num_width = self.total_stages.to_string().len();

        let mut lines: Vec<Line> = Vec::new();
        let mut prev_visual: Option<(&str, &str)> = None;

        for (i, stage) in self.stages.iter().enumerate() {
            let cur_visual = visual_groups[i].as_ref().map(|(kind, id)| (*kind, id.as_str()));

            // Emit group header on visual group transitions
            if cur_visual != prev_visual {
                // Open new visual group
                if let Some((kind, id)) = cur_visual {
                    if kind == "retry" {
                        let gname = id;
                        let attempt_str = if let Some(&(attempt, max)) = self.retry_attempts.get(gname) {
                            format!(" (attempt {}/{})", attempt + 1, max)
                        } else {
                            String::new()
                        };
                        lines.push(Line::from(vec![
                            Span::styled("  \u{256d}\u{2500} ", Style::default().fg(theme::TEXT_MUTED)),
                            Span::styled(
                                format!("{gname}{attempt_str}"),
                                Style::default().fg(theme::TEXT_MUTED),
                            ),
                        ]));
                    } else {
                        // parallel
                        lines.push(Line::from(vec![
                            Span::styled("  \u{256d}\u{2500} ", Style::default().fg(theme::TEXT_MUTED)),
                            Span::styled(
                                "parallel",
                                Style::default().fg(theme::TEXT_MUTED),
                            ),
                        ]));
                    }
                }
            }

            let in_group = cur_visual.is_some();
            let gutter = if in_group { "  \u{2502} " } else { "  " };

            let (icon, icon_color, detail) = match &stage.status {
                StageStatus::Pending => ("\u{00b7}", theme::TEXT_MUTED, "pending".to_string()),
                StageStatus::Building => ("\u{00b7}", theme::WARNING, "building context...".to_string()),
                StageStatus::Running {
                    elapsed,
                    activity_log,
                } => {
                    let spinner = SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()];
                    let detail = if let Some(act) = activity_log.last() {
                        format!("{} \u{00b7} {act}", format_duration(*elapsed))
                    } else {
                        format!("{} elapsed...", format_duration(*elapsed))
                    };
                    (spinner, theme::ACCENT, detail)
                }
                StageStatus::Completed {
                    duration,
                    artifacts,
                    activity_log,
                    ..
                } => {
                    if stage.verifying {
                        let spinner = SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()];
                        let detail = if let Some(act) = activity_log.last() {
                            format!("{} \u{00b7} verifying \u{00b7} {act}", format_duration_precise(*duration))
                        } else {
                            format!("{} \u{00b7} verifying\u{2026}", format_duration_precise(*duration))
                        };
                        (spinner, theme::WARNING, detail)
                    } else {
                        (
                            "\u{2713}",
                            theme::SUCCESS,
                            format!(
                                "{} \u{00b7} {}",
                                format_duration_precise(*duration),
                                artifacts.summary()
                            ),
                        )
                    }
                }
                StageStatus::Failed { .. } => ("\u{2717}", theme::ERROR, "failed".to_string()),
                StageStatus::Skipped { ref reason } => ("\u{2298}", theme::TEXT_MUTED, format!("skipped \u{00b7} {reason}")),
            };

            let is_selected = self.selection == SelectionTarget::Stage(i);
            let bg = if is_selected { theme::SURFACE_HIGHLIGHT } else { ratatui::style::Color::Reset };
            let name_style = if is_selected {
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD)
                    .bg(bg)
            } else {
                Style::default()
                    .fg(theme::TEXT_PRIMARY)
                    .add_modifier(Modifier::BOLD)
                    .bg(bg)
            };
            let select_indicator = if is_selected { "\u{25b8}" } else { " " };

            let detail_gutter = if in_group { "  \u{2502}   " } else { "    " };

            // Runtime + model label for the detail line
            let (model_short, model_short_color) = if !stage.model.is_empty() && stage.model != "commands" {
                let rt_label = format_runtime_short(&stage.runtime);
                let tools_label = if stage.allowed_tools.is_empty() {
                    String::new()
                } else {
                    format!(" tools: {}", stage.allowed_tools.join(", "))
                };
                (
                    format!(" [{} · {}]{}", rt_label, theme::shorten_model(&stage.model), tools_label),
                    runtime_color(&stage.runtime),
                )
            } else {
                (String::new(), theme::TOKEN_LABEL)
            };

            lines.push(Line::from(vec![
                Span::styled(gutter, Style::default().fg(theme::TEXT_MUTED).bg(bg)),
                Span::styled(select_indicator, Style::default().fg(theme::ACCENT).bg(bg)),
                Span::styled(icon, Style::default().fg(icon_color).bg(bg)),
                Span::styled(format!(" {:>num_width$}. ", i + 1), Style::default().fg(theme::TEXT_SECONDARY).bg(bg)),
                Span::styled(&stage.name, name_style),
            ]));
            lines.push(Line::from(vec![
                Span::styled(detail_gutter, Style::default().fg(theme::TEXT_MUTED)),
                Span::styled(format!("  {detail}"), Style::default().fg(theme::TEXT_MUTED)),
                Span::styled(model_short, Style::default().fg(model_short_color)),
            ]));

            // Emit verify step + group footer after the last stage in a retry group
            if let Some(ref gname) = stage.retry_group {
                let is_last_in_group = retry_group_ranges
                    .get(gname.as_str())
                    .is_some_and(|&(_, last)| i == last);
                if is_last_in_group {
                    // Render verify entry for this group
                    let v_selected = self.selection == SelectionTarget::Verify(gname.to_string());
                    let v_bg = if v_selected { theme::SURFACE_HIGHLIGHT } else { ratatui::style::Color::Reset };
                    let v_select_indicator = if v_selected { "\u{25b8}" } else { " " };

                    let (v_icon, v_color, v_detail) = if let Some(vs) = self.verify_states.get(gname.as_str()) {
                        match vs {
                            VerifyStatus::Pending => ("\u{00b7}", theme::TEXT_MUTED, "pending".to_string()),
                            VerifyStatus::Running { activity_log } => {
                                let spinner = SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()];
                                let detail = if let Some(act) = activity_log.last() {
                                    act.clone()
                                } else {
                                    "running\u{2026}".to_string()
                                };
                                (spinner, theme::WARNING, detail)
                            }
                            VerifyStatus::Passed => ("\u{2713}", theme::SUCCESS, "passed".to_string()),
                            VerifyStatus::Failed { response } => {
                                let short = if response.chars().count() > 60 {
                                    format!("{}\u{2026}", truncate_chars(response, 60).trim_end_matches("..."))
                                } else {
                                    response.clone()
                                };
                                ("\u{2717}", theme::ERROR, format!("failed \u{2014} {short}"))
                            }
                        }
                    } else {
                        ("\u{00b7}", theme::TEXT_MUTED, "pending".to_string())
                    };

                    let v_name_style = if v_selected {
                        Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD).bg(v_bg)
                    } else {
                        Style::default().fg(v_color).add_modifier(Modifier::BOLD).bg(v_bg)
                    };

                    lines.push(Line::from(vec![
                        Span::styled("  \u{2502} ", Style::default().fg(theme::TEXT_MUTED).bg(v_bg)),
                        Span::styled(v_select_indicator, Style::default().fg(theme::ACCENT).bg(v_bg)),
                        Span::styled(v_icon, Style::default().fg(v_color).bg(v_bg)),
                        Span::styled(" Verify", v_name_style),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("  \u{2502}     ", Style::default().fg(theme::TEXT_MUTED)),
                        Span::styled(format!("  {v_detail}"), Style::default().fg(theme::TEXT_MUTED)),
                    ]));
                    lines.push(Line::from(Span::styled(
                        "  \u{2570}\u{2500}",
                        Style::default().fg(theme::TEXT_MUTED),
                    )));
                }
            }

            // Emit parallel group footer after the last stage in a parallel group
            // (only if not in a retry group, since retry takes visual precedence)
            if stage.retry_group.is_none() {
                if let Some(pg) = stage.parallel_group {
                    let is_last_in_group = parallel_group_ranges
                        .get(&pg)
                        .is_some_and(|&(_, last)| i == last);
                    if is_last_in_group {
                        lines.push(Line::from(Span::styled(
                            "  \u{2570}\u{2500}",
                            Style::default().fg(theme::TEXT_MUTED),
                        )));
                    }
                }
            }

            prev_visual = cur_visual;
        }

        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, inner);
    }

    fn render_stage_detail(&mut self, frame: &mut Frame, area: Rect) {
        match &self.selection {
            SelectionTarget::Stage(idx) => {
                let detail_idx = *idx;
                self.render_stage_detail_for_stage(frame, area, detail_idx);
            }
            SelectionTarget::Verify(gname) => {
                let gname = gname.clone();
                self.render_verify_detail(frame, area, &gname);
            }
        }
    }

    fn render_verify_detail(&mut self, frame: &mut Frame, area: Rect, group_name: &str) {
        let title = format!("Verify: {group_name}");
        let block = theme::styled_block(&title, self.detail_focused);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut lines: Vec<Line> = Vec::new();

        let vs = self.verify_states.get(group_name);
        let (label, label_color) = match vs {
            Some(VerifyStatus::Pending) | None => ("  Status: pending", theme::TEXT_MUTED),
            Some(VerifyStatus::Running { .. }) => ("  Status: running\u{2026}", theme::WARNING),
            Some(VerifyStatus::Passed) => ("  Status: passed \u{2713}", theme::SUCCESS),
            Some(VerifyStatus::Failed { .. }) => ("  Status: failed \u{2717}", theme::ERROR),
        };
        lines.push(Line::from(Span::styled(
            label,
            Style::default().fg(label_color).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        if let Some(VerifyStatus::Running { activity_log }) = vs {
            if !activity_log.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  Activity:",
                    Style::default().fg(theme::TEXT_MUTED),
                )));
                for entry in activity_log {
                    let style = if entry.starts_with("$ ") {
                        Style::default().fg(theme::WARNING)
                    } else {
                        Style::default().fg(theme::ACCENT)
                    };
                    lines.push(Line::from(Span::styled(
                        format!("  {entry}"),
                        style,
                    )));
                }
            }
        }

        if let Some(VerifyStatus::Failed { response }) = vs {
            lines.push(Line::from(Span::styled(
                "  Output:",
                Style::default().fg(theme::TEXT_MUTED),
            )));
            for line in response.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {line}"),
                    Style::default().fg(theme::ERROR),
                )));
            }
        }

        let visible_height = inner.height as usize;
        let total_lines = lines.len();
        self.detail_content_height = total_lines;

        let scroll = if self.detail_scroll_pinned {
            let max_scroll = total_lines.saturating_sub(visible_height) as u16;
            self.detail_scroll.min(max_scroll)
        } else {
            let auto = if total_lines > visible_height {
                (total_lines - visible_height) as u16
            } else {
                0
            };
            self.detail_scroll = auto;
            auto
        };

        let paragraph = Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .scroll((scroll, 0));
        frame.render_widget(paragraph, inner);

        // Scrollbar
        if total_lines > visible_height {
            let mut scrollbar_state = ScrollbarState::new(total_lines)
                .position(scroll as usize);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .style(Style::default().fg(theme::TEXT_MUTED)),
                area,
                &mut scrollbar_state,
            );
        }
    }

    fn render_stage_detail_for_stage(&mut self, frame: &mut Frame, area: Rect, detail_idx: usize) {

        let Some(stage) = self.stages.get(detail_idx) else {
            let block = theme::styled_block("Stage Detail", self.detail_focused);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let waiting = Paragraph::new(Span::styled(
                "  Waiting for pipeline to start...",
                Style::default().fg(theme::TEXT_SECONDARY),
            ));
            frame.render_widget(waiting, inner);
            return;
        };

        let title_suffix = if self.show_prompt { " [Prompt]" } else { "" };
        let title = format!(
            "Stage {}/{}: {}{}",
            stage.index + 1,
            self.total_stages,
            stage.name,
            title_suffix,
        );
        let block = theme::styled_block(&title, self.detail_focused);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Prompt/context view (toggled with 'p')
        if self.show_prompt {
            let mut lines: Vec<Line> = Vec::new();

            if let Some(ref summary) = stage.prior_summary {
                lines.push(Line::from(Span::styled(
                    "  Prior Summary:",
                    Style::default().fg(theme::WARNING).add_modifier(Modifier::BOLD),
                )));
                for l in summary.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {l}"),
                        Style::default().fg(theme::TEXT_MUTED),
                    )));
                }
                lines.push(Line::from(""));
            }

            if let Some(ref prompt) = stage.rendered_prompt {
                lines.push(Line::from(Span::styled(
                    "  Rendered Prompt:",
                    Style::default().fg(theme::WARNING).add_modifier(Modifier::BOLD),
                )));
                for l in prompt.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {l}"),
                        Style::default().fg(theme::TEXT_PRIMARY),
                    )));
                }
            } else {
                lines.push(Line::from(Span::styled(
                    "  No prompt available (command stage or not yet built)",
                    Style::default().fg(theme::TEXT_MUTED),
                )));
            }

            let visible_height = inner.height as usize;
            let wrap_width = inner.width.max(1) as usize;
            // Estimate wrapped line count: each logical line may span multiple visual lines.
            let total_lines: usize = lines
                .iter()
                .map(|line| {
                    let char_width: usize = line.spans.iter().map(|s| s.content.len()).sum();
                    if char_width == 0 {
                        1
                    } else {
                        char_width.div_ceil(wrap_width)
                    }
                })
                .sum();
            self.detail_content_height = total_lines;

            let scroll = if self.detail_scroll_pinned {
                let max_scroll = total_lines.saturating_sub(visible_height) as u16;
                self.detail_scroll.min(max_scroll)
            } else {
                0
            };

            let paragraph = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0));
            frame.render_widget(paragraph, inner);

            // Scrollbar
            if total_lines > visible_height {
                let mut scrollbar_state = ScrollbarState::new(total_lines)
                    .position(scroll as usize);
                frame.render_stateful_widget(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight)
                        .style(Style::default().fg(theme::TEXT_MUTED)),
                    area,
                    &mut scrollbar_state,
                );
            }
            return;
        }

        let detail_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // stage info
                Constraint::Min(0),    // activity log / artifacts / error
            ])
            .split(inner);

        // Stage info header
        let mut info_lines = if stage.model == "commands" {
            vec![Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled("type: commands", Style::default().fg(theme::TOKEN_LABEL)),
            ])]
        } else {
            let rt_color = runtime_color(&stage.runtime);
            let mut header_spans = vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    format_runtime_short(&stage.runtime),
                    Style::default().fg(rt_color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" · ", Style::default().fg(theme::TEXT_MUTED)),
                Span::styled(
                    theme::shorten_model(&stage.model),
                    Style::default().fg(theme::TOKEN_LABEL),
                ),
            ];
            if !stage.allowed_tools.is_empty() {
                header_spans.push(Span::styled(
                    format!("  tools: {}", stage.allowed_tools.join(", ")),
                    Style::default().fg(theme::TEXT_MUTED),
                ));
            }
            if stage.isolated {
                header_spans.push(Span::styled(
                    "  [isolated]",
                    Style::default().fg(theme::TEXT_MUTED),
                ));
            }
            vec![Line::from(header_spans)]
        };

        match &stage.status {
            StageStatus::Running { elapsed, .. } => {
                info_lines.push(Line::from(vec![
                    Span::styled("  Elapsed: ", Style::default().fg(theme::TEXT_MUTED)),
                    Span::styled(
                        format_duration(*elapsed),
                        Style::default().fg(theme::WARNING),
                    ),
                ]));
            }
            StageStatus::Completed { duration, .. } => {
                info_lines.push(Line::from(vec![
                    Span::styled("  Completed in ", Style::default().fg(theme::SUCCESS)),
                    Span::styled(
                        format_duration_precise(*duration),
                        Style::default()
                            .fg(theme::SUCCESS)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
            StageStatus::Skipped { reason } => {
                info_lines.push(Line::from(vec![
                    Span::styled("  Skipped: ", Style::default().fg(theme::TEXT_MUTED)),
                    Span::styled(
                        reason.clone(),
                        Style::default().fg(theme::TEXT_MUTED),
                    ),
                ]));
            }
            _ => {}
        }

        // Branch selection info
        if let Some((ref selected_route, ref available_routes)) = stage.branch_info {
            info_lines.push(Line::from(vec![
                Span::styled("  Branch: ", Style::default().fg(theme::TEXT_MUTED)),
                Span::styled(
                    selected_route.clone(),
                    Style::default().fg(theme::SUCCESS).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" (from: {})", available_routes.join(", ")),
                    Style::default().fg(theme::TEXT_MUTED),
                ),
            ]));
        }

        frame.render_widget(Paragraph::new(info_lines), detail_chunks[0]);

        // Error detail
        if let StageStatus::Failed { error } = &stage.status {
            let error_paragraph = Paragraph::new(format!("  Error:\n  {error}"))
                .style(Style::default().fg(theme::ERROR))
                .wrap(ratatui::widgets::Wrap { trim: false });
            frame.render_widget(error_paragraph, detail_chunks[1]);
            return;
        }

        // Skipped detail — show reason and return early
        if let StageStatus::Skipped { reason } = &stage.status {
            let skip_paragraph = Paragraph::new(format!("  Skipped: {reason}"))
                .style(Style::default().fg(theme::TEXT_MUTED))
                .wrap(ratatui::widgets::Wrap { trim: false });
            frame.render_widget(skip_paragraph, detail_chunks[1]);
            return;
        }

        // Activity log + artifacts in the remaining space
        let mut detail_lines: Vec<Line> = Vec::new();

        // Get activity log from running or completed state
        let activity_log = match &stage.status {
            StageStatus::Running { activity_log, .. } => Some(activity_log),
            StageStatus::Completed { activity_log, .. } => Some(activity_log),
            _ => None,
        };

        if let Some(log) = activity_log {
            if !log.is_empty() {
                let label = if stage.model == "commands" { "  Output:" } else { "  Activity:" };
                detail_lines.push(Line::from(Span::styled(
                    label,
                    Style::default().fg(theme::TEXT_MUTED),
                )));
                for entry in log {
                    let style = if entry.starts_with("$ ") {
                        Style::default().fg(theme::WARNING)
                    } else {
                        Style::default().fg(theme::ACCENT)
                    };
                    detail_lines.push(Line::from(Span::styled(
                        format!("  {entry}"),
                        style,
                    )));
                }
            }
        }

        // Artifacts (for completed stages)
        if let StageStatus::Completed {
            artifacts,
            token_usage,
            ..
        } = &stage.status
        {
            if !artifacts.changes.is_empty() {
                detail_lines.push(Line::from(""));
                detail_lines.push(Line::from(Span::styled(
                    "  Files changed:",
                    Style::default().fg(theme::TEXT_MUTED),
                )));
                for change in &artifacts.changes {
                    let (prefix, color) = match change.kind {
                        fujin_core::artifact::FileChangeKind::Created => ("+", theme::SUCCESS),
                        fujin_core::artifact::FileChangeKind::Modified => ("~", theme::WARNING),
                        fujin_core::artifact::FileChangeKind::Deleted => ("-", theme::ERROR),
                    };
                    let size_str = change.size.map(format_size).unwrap_or_default();
                    detail_lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(prefix, Style::default().fg(color)),
                        Span::styled(
                            format!(" {}", change.path.display()),
                            Style::default().fg(theme::TEXT_PRIMARY),
                        ),
                        Span::styled(
                            format!("  {size_str}"),
                            Style::default().fg(theme::TEXT_MUTED),
                        ),
                    ]));
                }
            }
            if let Some(usage) = token_usage {
                detail_lines.push(Line::from(""));
                detail_lines.push(Line::from(vec![
                    Span::styled("  Tokens: ", Style::default().fg(theme::TEXT_MUTED)),
                    Span::styled(
                        format!("{} in", format_number(usage.input_tokens)),
                        Style::default().fg(theme::TOKEN_LABEL),
                    ),
                    Span::styled(" / ", Style::default().fg(theme::TEXT_MUTED)),
                    Span::styled(
                        format!("{} out", format_number(usage.output_tokens)),
                        Style::default().fg(theme::TOKEN_LABEL),
                    ),
                ]));
            }
        }

        // Exported variables
        if !stage.exported_vars.is_empty() {
            detail_lines.push(Line::from(""));
            detail_lines.push(Line::from(Span::styled(
                "  Exports:",
                Style::default().fg(theme::TEXT_MUTED),
            )));
            for (key, value) in &stage.exported_vars {
                let display_val = truncate_chars(value, 60);
                detail_lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {key}"),
                        Style::default().fg(theme::TEXT_SECONDARY),
                    ),
                    Span::styled(" = ", Style::default().fg(theme::TEXT_MUTED)),
                    Span::styled(
                        format!("\"{display_val}\""),
                        Style::default().fg(theme::TEXT_PRIMARY),
                    ),
                ]));
            }
        }

        if !detail_lines.is_empty() {
            let visible_height = detail_chunks[1].height as usize;
            let total_lines = detail_lines.len();

            self.detail_content_height = total_lines;

            let scroll = if self.detail_scroll_pinned {
                let max_scroll = total_lines.saturating_sub(visible_height) as u16;
                self.detail_scroll.min(max_scroll)
            } else {
                // Auto-scroll to bottom
                let auto = if total_lines > visible_height {
                    (total_lines - visible_height) as u16
                } else {
                    0
                };
                self.detail_scroll = auto;
                auto
            };

            let paragraph = Paragraph::new(detail_lines)
                .wrap(ratatui::widgets::Wrap { trim: false })
                .scroll((scroll, 0));
            frame.render_widget(paragraph, detail_chunks[1]);

            // Scrollbar
            if total_lines > visible_height {
                let mut scrollbar_state = ScrollbarState::new(total_lines)
                    .position(scroll as usize);
                frame.render_stateful_widget(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight)
                        .style(Style::default().fg(theme::TEXT_MUTED)),
                    area,
                    &mut scrollbar_state,
                );
            }
        }
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let mut constraints: Vec<Constraint> = vec![Constraint::Length(1)]; // separator
        if self.finished && self.token_usage_by_model.len() > 1 {
            // Per-model lines + total line
            for _ in 0..self.token_usage_by_model.len() + 1 {
                constraints.push(Constraint::Length(1));
            }
        }
        constraints.push(Constraint::Length(1)); // controls line

        let sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let sep = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(theme::BORDER));
        frame.render_widget(sep, sub[0]);

        let elapsed = self.final_elapsed.unwrap_or_else(|| self.start_time.elapsed());

        // Show cancel confirmation warning
        if self.confirm_cancel {
            let footer = Line::from(vec![
                Span::styled("  \u{26a0} ", Style::default().fg(theme::WARNING)),
                Span::styled(
                    "Press q/y again to cancel pipeline, any other key to dismiss",
                    Style::default().fg(theme::WARNING).add_modifier(Modifier::BOLD),
                ),
            ]);
            frame.render_widget(Paragraph::new(footer), sub[1]);
            return;
        }

        // Per-model token breakdown (only when finished and >1 model)
        let controls_row = if self.finished && self.token_usage_by_model.len() > 1 {
            for (i, (model, usage)) in self.token_usage_by_model.iter().enumerate() {
                let model_short = theme::shorten_model(model);
                let line = Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        format!("{:<16}", model_short),
                        Style::default().fg(theme::ACCENT),
                    ),
                    Span::styled(
                        format!("{:>8} in", format_number(usage.input_tokens)),
                        Style::default().fg(theme::TOKEN_LABEL),
                    ),
                    Span::styled(" / ", Style::default().fg(theme::TEXT_MUTED)),
                    Span::styled(
                        format!("{:>8} out", format_number(usage.output_tokens)),
                        Style::default().fg(theme::TOKEN_LABEL),
                    ),
                ]);
                frame.render_widget(Paragraph::new(line), sub[1 + i]);
            }
            // Total line
            let total_line = Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    format!("{:<16}", "Total"),
                    Style::default()
                        .fg(theme::TEXT_PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:>8} in", format_number(self.total_input_tokens)),
                    Style::default().fg(theme::TOKEN_LABEL).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" / ", Style::default().fg(theme::TEXT_MUTED)),
                Span::styled(
                    format!("{:>8} out", format_number(self.total_output_tokens)),
                    Style::default().fg(theme::TOKEN_LABEL).add_modifier(Modifier::BOLD),
                ),
            ]);
            let total_idx = 1 + self.token_usage_by_model.len();
            frame.render_widget(Paragraph::new(total_line), sub[total_idx]);
            total_idx + 1
        } else {
            1 // controls go in sub[1]
        };

        // Controls line
        let mut spans = vec![Span::raw("  ")];

        // Show inline total when not showing per-model breakdown
        if !self.finished || self.token_usage_by_model.len() <= 1 {
            spans.push(Span::styled("Tokens: ", Style::default().fg(theme::TEXT_MUTED)));
            if self.token_usage_by_model.len() == 1 {
                let (model, usage) = &self.token_usage_by_model[0];
                let model_short = theme::shorten_model(model);
                spans.push(Span::styled(
                    format!("{model_short}: "),
                    Style::default().fg(theme::ACCENT),
                ));
                spans.push(Span::styled(
                    format!(
                        "{} in / {} out",
                        format_number(usage.input_tokens),
                        format_number(usage.output_tokens)
                    ),
                    Style::default().fg(theme::TOKEN_LABEL),
                ));
            } else {
                spans.push(Span::styled(
                    format!(
                        "{} in / {} out",
                        format_number(self.total_input_tokens),
                        format_number(self.total_output_tokens)
                    ),
                    Style::default().fg(theme::TOKEN_LABEL),
                ));
            }
            spans.push(Span::raw("    "));
        }

        if self.detail_focused {
            spans.push(Span::styled("[Esc]", Style::default().fg(theme::ACCENT)));
            spans.push(Span::styled(" Back  ", Style::default().fg(theme::TEXT_SECONDARY)));
            spans.push(Span::styled("[j/k]", Style::default().fg(theme::ACCENT)));
            spans.push(Span::styled(" Stages  ", Style::default().fg(theme::TEXT_SECONDARY)));
            spans.push(Span::styled("[PgUp/Dn]", Style::default().fg(theme::ACCENT)));
            spans.push(Span::styled(" Scroll  ", Style::default().fg(theme::TEXT_SECONDARY)));
            if self.detail_content_height > 0 {
                let pos = self.detail_scroll as usize + 1;
                let total = self.detail_content_height;
                spans.push(Span::styled(
                    format!(" {pos}/{total}"),
                    Style::default().fg(theme::TEXT_MUTED),
                ));
            }
        } else if self.finished {
            spans.push(Span::styled("[b]", Style::default().fg(theme::ACCENT)));
            spans.push(Span::styled(" Back  ", Style::default().fg(theme::TEXT_SECONDARY)));
            spans.push(Span::styled("[j/k]", Style::default().fg(theme::ACCENT)));
            spans.push(Span::styled(" Stages  ", Style::default().fg(theme::TEXT_SECONDARY)));
            spans.push(Span::styled("[f]", Style::default().fg(theme::ACCENT)));
            spans.push(Span::styled(" Focus  ", Style::default().fg(theme::TEXT_SECONDARY)));
            spans.push(Span::styled("[p]", Style::default().fg(theme::ACCENT)));
            spans.push(Span::styled(" Prompt  ", Style::default().fg(theme::TEXT_SECONDARY)));
            spans.push(Span::styled("[q]", Style::default().fg(theme::ACCENT)));
            spans.push(Span::styled(" Quit", Style::default().fg(theme::TEXT_SECONDARY)));
        } else {
            spans.push(Span::styled("[j/k]", Style::default().fg(theme::ACCENT)));
            spans.push(Span::styled(" Stages  ", Style::default().fg(theme::TEXT_SECONDARY)));
            spans.push(Span::styled("[f]", Style::default().fg(theme::ACCENT)));
            spans.push(Span::styled(" Focus  ", Style::default().fg(theme::TEXT_SECONDARY)));
            spans.push(Span::styled("[p]", Style::default().fg(theme::ACCENT)));
            spans.push(Span::styled(" Prompt  ", Style::default().fg(theme::TEXT_SECONDARY)));
            spans.push(Span::styled("[q]", Style::default().fg(theme::WARNING)));
            spans.push(Span::styled(" Stop", Style::default().fg(theme::TEXT_SECONDARY)));
        }

        spans.push(Span::styled(
            format!("  Total: {}", format_duration(elapsed)),
            Style::default().fg(theme::TEXT_MUTED),
        ));

        let footer = Line::from(spans);
        frame.render_widget(Paragraph::new(footer), sub[controls_row]);
    }
}

fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    if mins > 0 {
        format!("{mins}m{secs:02}s")
    } else if total_secs == 0 {
        // Sub-second: show fractional
        let ms = d.as_millis();
        if ms > 0 {
            format!("{:.1}s", d.as_secs_f64())
        } else {
            "0s".to_string()
        }
    } else {
        format!("{secs}s")
    }
}

/// Format duration always showing one decimal place (for completed stage times).
fn format_duration_precise(d: Duration) -> String {
    let total_secs = d.as_secs();
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    if mins > 0 {
        format!("{mins}m{secs:02}s")
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1}mb", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1}kb", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}b")
    }
}

fn format_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{},{:03}", n / 1000, n % 1000)
    } else {
        n.to_string()
    }
}

/// Short display label for a runtime name.
fn format_runtime_short(runtime: &str) -> &str {
    match runtime {
        "claude-code" | "" => "claude",
        "copilot-cli" => "copilot",
        other => other,
    }
}

/// Color for a runtime label.
fn runtime_color(runtime: &str) -> ratatui::style::Color {
    match runtime {
        "copilot-cli" => ratatui::style::Color::Rgb(110, 200, 250), // light blue
        _ => theme::TOKEN_LABEL, // purple for claude-code / default
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fujin_core::artifact::ArtifactSet;

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn make_ctrl_c() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    }

    fn new_state() -> ExecutionState {
        ExecutionState::new(
            "Test Pipeline".into(),
            String::new(),
            3,
            vec![
                ("s0".into(), "Stage 0".into(), String::new(), String::new(), None, None),
                ("s1".into(), "Stage 1".into(), String::new(), String::new(), None, None),
                ("s2".into(), "Stage 2".into(), String::new(), String::new(), None, None),
            ],
        )
    }

    // --- Key handling tests ---

    #[test]
    fn test_ctrl_c_cancels_while_running() {
        let mut state = new_state();
        assert!(matches!(
            state.handle_key(make_ctrl_c()),
            ExecutionAction::CancelPipeline
        ));
    }

    #[test]
    fn test_ctrl_c_quits_when_finished() {
        let mut state = new_state();
        state.finished = true;
        assert!(matches!(
            state.handle_key(make_ctrl_c()),
            ExecutionAction::Quit
        ));
    }

    #[test]
    fn test_q_shows_confirm_then_cancels() {
        let mut state = new_state();
        // First q shows confirm
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Char('q'))),
            ExecutionAction::None
        ));
        assert!(state.confirm_cancel);
        // Second q cancels
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Char('q'))),
            ExecutionAction::CancelPipeline
        ));
        assert!(!state.confirm_cancel);
    }

    #[test]
    fn test_q_confirm_dismissed_by_other_key() {
        let mut state = new_state();
        state.handle_key(make_key(KeyCode::Char('q')));
        assert!(state.confirm_cancel);
        // Any other key dismisses
        state.handle_key(make_key(KeyCode::Char('j')));
        assert!(!state.confirm_cancel);
    }

    #[test]
    fn test_q_quits_when_finished() {
        let mut state = new_state();
        state.finished = true;
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Char('q'))),
            ExecutionAction::Quit
        ));
    }

    #[test]
    fn test_b_back_to_browser_when_finished() {
        let mut state = new_state();
        state.finished = true;
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Char('b'))),
            ExecutionAction::BackToBrowser
        ));
    }

    #[test]
    fn test_b_ignored_while_running() {
        let mut state = new_state();
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Char('b'))),
            ExecutionAction::None
        ));
    }

    #[test]
    fn test_tab_toggles_detail_focus() {
        let mut state = new_state();
        assert!(!state.detail_focused);
        state.handle_key(make_key(KeyCode::Tab));
        assert!(state.detail_focused);
        state.handle_key(make_key(KeyCode::Tab));
        assert!(!state.detail_focused);
    }

    #[test]
    fn test_jk_navigation_when_finished() {
        let mut state = new_state();
        state.finished = true;
        for stage in &mut state.stages {
            stage.status = StageStatus::Completed {
                duration: Duration::from_secs(1),
                artifacts: ArtifactSet::default(),
                token_usage: None,
                activity_log: vec![],
            };
        }
        state.selection = SelectionTarget::Stage(0);

        state.handle_key(make_key(KeyCode::Char('j')));
        assert_eq!(state.selection, SelectionTarget::Stage(1));

        state.handle_key(make_key(KeyCode::Down));
        assert_eq!(state.selection, SelectionTarget::Stage(2));

        // Can't go past end
        state.handle_key(make_key(KeyCode::Char('j')));
        assert_eq!(state.selection, SelectionTarget::Stage(2));

        state.handle_key(make_key(KeyCode::Char('k')));
        assert_eq!(state.selection, SelectionTarget::Stage(1));

        state.handle_key(make_key(KeyCode::Up));
        assert_eq!(state.selection, SelectionTarget::Stage(0));

        // Can't go before start
        state.handle_key(make_key(KeyCode::Char('k')));
        assert_eq!(state.selection, SelectionTarget::Stage(0));
    }

    #[test]
    fn test_g_jumps_to_first() {
        let mut state = new_state();
        state.selection = SelectionTarget::Stage(2);
        state.handle_key(make_key(KeyCode::Char('g')));
        assert_eq!(state.selection, SelectionTarget::Stage(0));
    }

    #[test]
    fn test_big_g_jumps_to_last() {
        let mut state = new_state();
        state.selection = SelectionTarget::Stage(0);
        state.handle_key(make_key(KeyCode::Char('G')));
        assert_eq!(state.selection, SelectionTarget::Stage(2));
    }

    // --- Pipeline event handling tests ---

    #[test]
    fn test_stage_started_creates_stage_info() {
        let mut state = new_state();
        state.handle_pipeline_event(PipelineEvent::StageStarted {
            stage_index: 0,
            stage_id: "build".into(),
            stage_name: "Build Code".into(),
            model: "sonnet".into(),
            runtime: "claude-code".into(),
            allowed_tools: vec![],
            retry_group: None,
            isolated: false,
        });

        assert_eq!(state.stages.len(), 3); // pre-populated by new_state()
        assert_eq!(state.stages[0].id, "build");
        assert_eq!(state.stages[0].name, "Build Code");
        assert_eq!(state.active_stage, Some(0));
    }

    #[test]
    fn test_stage_completed_tracks_tokens() {
        let mut state = new_state();
        state.handle_pipeline_event(PipelineEvent::StageStarted {
            stage_index: 0,
            stage_id: "s0".into(),
            stage_name: "S0".into(),
            model: "m".into(),
            runtime: "claude-code".into(),
            allowed_tools: vec![],
            retry_group: None,
            isolated: false,
        });
        state.stages[0].status = StageStatus::Running {
            elapsed: Duration::ZERO,
            activity_log: vec![],
        };

        state.handle_pipeline_event(PipelineEvent::StageCompleted {
            stage_index: 0,
            stage_id: "s0".into(),
            duration: Duration::from_secs(5),
            artifacts: ArtifactSet::default(),
            token_usage: Some(TokenUsage {
                input_tokens: 1000,
                output_tokens: 500,
            }),
        });

        assert_eq!(state.total_input_tokens, 1000);
        assert_eq!(state.total_output_tokens, 500);
        assert!(state.active_stage.is_none());
        assert!(matches!(state.stages[0].status, StageStatus::Completed { .. }));
    }

    #[test]
    fn test_stage_failed_sets_status() {
        let mut state = new_state();
        state.handle_pipeline_event(PipelineEvent::StageStarted {
            stage_index: 0,
            stage_id: "s0".into(),
            stage_name: "S0".into(),
            model: "m".into(),
            runtime: "claude-code".into(),
            allowed_tools: vec![],
            retry_group: None,
            isolated: false,
        });

        state.handle_pipeline_event(PipelineEvent::StageFailed {
            stage_index: 0,
            stage_id: "s0".into(),
            error: "boom".into(),
            checkpoint_saved: true,
        });

        assert!(matches!(state.stages[0].status, StageStatus::Failed { .. }));
        assert!(state.active_stage.is_none());
    }

    #[test]
    fn test_pipeline_completed_sets_finished() {
        let mut state = new_state();
        state.handle_pipeline_event(PipelineEvent::PipelineCompleted {
            total_duration: Duration::from_secs(30),
            stages_completed: 3,
            token_usage_by_model: vec![],
        });

        assert!(state.finished);
        assert!(state.final_message.is_some());
        assert!(state.final_message.unwrap().contains("30.0s"));
    }

    #[test]
    fn test_pipeline_cancelled_sets_finished() {
        let mut state = new_state();
        state.handle_pipeline_event(PipelineEvent::StageStarted {
            stage_index: 1,
            stage_id: "s1".into(),
            stage_name: "S1".into(),
            model: "m".into(),
            runtime: "claude-code".into(),
            allowed_tools: vec![],
            retry_group: None,
            isolated: false,
        });

        state.handle_pipeline_event(PipelineEvent::PipelineCancelled { stage_index: 1 });

        assert!(state.finished);
        assert!(matches!(state.stages[1].status, StageStatus::Failed { .. }));
    }

    #[test]
    fn test_agent_activity_appends_to_log() {
        let mut state = new_state();
        state.handle_pipeline_event(PipelineEvent::StageStarted {
            stage_index: 0,
            stage_id: "s0".into(),
            stage_name: "S0".into(),
            model: "m".into(),
            runtime: "claude-code".into(),
            allowed_tools: vec![],
            retry_group: None,
            isolated: false,
        });
        state.handle_pipeline_event(PipelineEvent::AgentRunning {
            stage_index: 0,
            stage_id: "s0".into(),
        });

        state.handle_pipeline_event(PipelineEvent::AgentActivity {
            stage_index: 0,
            activity: "Tool: Read \u{2014} src/main.rs".into(),
        });
        state.handle_pipeline_event(PipelineEvent::AgentActivity {
            stage_index: 0,
            activity: "Tool: Write \u{2014} output.txt".into(),
        });

        if let StageStatus::Running { activity_log, .. } = &state.stages[0].status {
            assert_eq!(activity_log.len(), 2);
            assert_eq!(activity_log[0], "Tool: Read \u{2014} src/main.rs");
        } else {
            panic!("Expected Running status");
        }
    }

    #[test]
    fn test_completed_preserves_activity_log() {
        let mut state = new_state();
        state.handle_pipeline_event(PipelineEvent::StageStarted {
            stage_index: 0,
            stage_id: "s0".into(),
            stage_name: "S0".into(),
            model: "m".into(),
            runtime: "claude-code".into(),
            allowed_tools: vec![],
            retry_group: None,
            isolated: false,
        });
        state.handle_pipeline_event(PipelineEvent::AgentRunning {
            stage_index: 0,
            stage_id: "s0".into(),
        });
        state.handle_pipeline_event(PipelineEvent::AgentActivity {
            stage_index: 0,
            activity: "did something".into(),
        });

        state.handle_pipeline_event(PipelineEvent::StageCompleted {
            stage_index: 0,
            stage_id: "s0".into(),
            duration: Duration::from_secs(1),
            artifacts: ArtifactSet::default(),
            token_usage: None,
        });

        if let StageStatus::Completed { activity_log, .. } = &state.stages[0].status {
            assert_eq!(activity_log.len(), 1);
            assert_eq!(activity_log[0], "did something");
        } else {
            panic!("Expected Completed status with preserved log");
        }
    }

    // --- Formatting tests ---

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(Duration::from_secs(5)), "5s");
        assert_eq!(format_duration(Duration::from_secs(65)), "1m05s");
        assert_eq!(format_duration(Duration::from_secs(3661)), "61m01s");
    }

    #[test]
    fn test_format_duration_subsecond() {
        assert_eq!(format_duration(Duration::from_millis(300)), "0.3s");
        assert_eq!(format_duration(Duration::from_millis(0)), "0s");
    }

    #[test]
    fn test_format_duration_precise() {
        assert_eq!(format_duration_precise(Duration::from_millis(300)), "0.3s");
        assert_eq!(format_duration_precise(Duration::from_secs(5)), "5.0s");
        assert_eq!(format_duration_precise(Duration::from_secs(65)), "1m05s");
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(500), "500b");
        assert_eq!(format_size(2048), "2.0kb");
        assert_eq!(format_size(1_500_000), "1.4mb");
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(42), "42");
        assert_eq!(format_number(1500), "1,500");
        assert_eq!(format_number(2_500_000), "2.5M");
    }

    // --- Pipeline event handler tests ---

    fn start_stage(state: &mut ExecutionState, index: usize) {
        state.handle_pipeline_event(PipelineEvent::StageStarted {
            stage_index: index,
            stage_id: format!("s{index}"),
            stage_name: format!("S{index}"),
            model: "m".into(),
            runtime: "claude-code".into(),
            allowed_tools: vec![],
            retry_group: None,
            isolated: false,
        });
    }

    #[test]
    fn test_context_building_sets_status() {
        let mut state = new_state();
        start_stage(&mut state, 0);
        state.handle_pipeline_event(PipelineEvent::ContextBuilding { stage_index: 0 });
        assert!(matches!(state.stages[0].status, StageStatus::Building));
    }

    #[test]
    fn test_stage_context_stores_prompt_and_summary() {
        let mut state = new_state();
        start_stage(&mut state, 0);
        state.handle_pipeline_event(PipelineEvent::StageContext {
            stage_index: 0,
            rendered_prompt: "Do the thing".into(),
            prior_summary: Some("Previously: did stuff".into()),
        });
        assert_eq!(state.stages[0].rendered_prompt.as_deref(), Some("Do the thing"));
        assert_eq!(
            state.stages[0].prior_summary.as_deref(),
            Some("Previously: did stuff")
        );
    }

    #[test]
    fn test_agent_running_transitions_to_running() {
        let mut state = new_state();
        start_stage(&mut state, 0);
        state.handle_pipeline_event(PipelineEvent::AgentRunning {
            stage_index: 0,
            stage_id: "s0".into(),
        });
        assert!(matches!(
            state.stages[0].status,
            StageStatus::Running { .. }
        ));
    }

    #[test]
    fn test_agent_tick_updates_elapsed() {
        let mut state = new_state();
        start_stage(&mut state, 0);
        state.handle_pipeline_event(PipelineEvent::AgentRunning {
            stage_index: 0,
            stage_id: "s0".into(),
        });
        state.handle_pipeline_event(PipelineEvent::AgentTick {
            stage_index: 0,
            elapsed: Duration::from_secs(5),
        });
        if let StageStatus::Running { elapsed, .. } = &state.stages[0].status {
            assert_eq!(*elapsed, Duration::from_secs(5));
        } else {
            panic!("Expected Running status");
        }
    }

    #[test]
    fn test_command_running_appends_activity() {
        let mut state = new_state();
        start_stage(&mut state, 0);
        state.handle_pipeline_event(PipelineEvent::CommandRunning {
            stage_index: 0,
            command_index: 0,
            command: "echo hello".into(),
            total_commands: 2,
        });
        if let StageStatus::Running { activity_log, .. } = &state.stages[0].status {
            assert_eq!(activity_log.len(), 1);
            assert!(activity_log[0].contains("echo hello"));
            assert!(activity_log[0].contains("[1/2]"));
        } else {
            panic!("Expected Running status");
        }
    }

    #[test]
    fn test_command_output_appends_line() {
        let mut state = new_state();
        start_stage(&mut state, 0);
        state.handle_pipeline_event(PipelineEvent::AgentRunning {
            stage_index: 0,
            stage_id: "s0".into(),
        });
        state.handle_pipeline_event(PipelineEvent::CommandOutput {
            stage_index: 0,
            line: "some output".into(),
        });
        if let StageStatus::Running { activity_log, .. } = &state.stages[0].status {
            assert!(activity_log.contains(&"some output".to_string()));
        } else {
            panic!("Expected Running status");
        }
    }

    #[test]
    fn test_pipeline_failed_sets_finished() {
        let mut state = new_state();
        state.handle_pipeline_event(PipelineEvent::PipelineFailed {
            error: "something broke".into(),
        });
        assert!(state.finished);
        assert!(state.final_message.as_ref().unwrap().contains("something broke"));
    }

    #[test]
    fn test_retry_group_attempt_resets_stages() {
        let mut state = new_state();
        // Start and fail stage 0
        start_stage(&mut state, 0);
        state.stages[0].retry_group = Some("grp".into());
        state.stages[1].retry_group = Some("grp".into());

        state.handle_pipeline_event(PipelineEvent::RetryGroupAttempt {
            group_name: "grp".into(),
            attempt: 1,
            max_retries: 3,
            first_stage_index: 0,
            failed_stage_id: "s0".into(),
            error: "oops".into(),
        });
        // Failed stage is marked with retry info
        assert!(matches!(state.stages[0].status, StageStatus::Failed { .. }));
        // Subsequent stages reset to Pending
        assert!(matches!(state.stages[1].status, StageStatus::Pending));
        assert_eq!(state.retry_attempts.get("grp"), Some(&(1, 3)));
    }

    #[test]
    fn test_retry_limit_reached_auto_continues() {
        let mut state = new_state();
        let (tx, rx) = tokio::sync::oneshot::channel();
        state.handle_pipeline_event(PipelineEvent::RetryLimitReached {
            group_name: "grp".into(),
            total_attempts: 3,
            response_tx: tx,
        });
        // TUI auto-sends true
        assert_eq!(rx.blocking_recv().unwrap(), true);
    }

    #[test]
    fn test_verify_running_sets_state() {
        let mut state = new_state();
        start_stage(&mut state, 0);
        state.handle_pipeline_event(PipelineEvent::VerifyRunning {
            group_name: "grp".into(),
            stage_index: 0,
        });
        assert!(state.stages[0].verifying);
        assert!(matches!(
            state.verify_states.get("grp"),
            Some(VerifyStatus::Running { .. })
        ));
    }

    #[test]
    fn test_verify_passed_clears_verifying() {
        let mut state = new_state();
        start_stage(&mut state, 0);
        state.stages[0].verifying = true;
        state.handle_pipeline_event(PipelineEvent::VerifyPassed {
            group_name: "grp".into(),
            stage_index: 0,
        });
        assert!(!state.stages[0].verifying);
        assert!(matches!(
            state.verify_states.get("grp"),
            Some(VerifyStatus::Passed)
        ));
    }

    #[test]
    fn test_verify_failed_stores_response() {
        let mut state = new_state();
        start_stage(&mut state, 0);
        state.stages[0].verifying = true;
        state.handle_pipeline_event(PipelineEvent::VerifyFailed {
            group_name: "grp".into(),
            stage_index: 0,
            response: "Tests are failing".into(),
        });
        assert!(!state.stages[0].verifying);
        if let Some(VerifyStatus::Failed { response }) = state.verify_states.get("grp") {
            assert_eq!(response, "Tests are failing");
        } else {
            panic!("Expected VerifyStatus::Failed");
        }
    }

    #[test]
    fn test_verify_activity_appends_to_running() {
        let mut state = new_state();
        start_stage(&mut state, 0);
        state.handle_pipeline_event(PipelineEvent::VerifyRunning {
            group_name: "grp".into(),
            stage_index: 0,
        });
        state.handle_pipeline_event(PipelineEvent::VerifyActivity {
            stage_index: 0,
            activity: "checking tests...".into(),
        });
        if let Some(VerifyStatus::Running { activity_log }) = state.verify_states.get("grp") {
            assert_eq!(activity_log, &["checking tests..."]);
        } else {
            panic!("Expected VerifyStatus::Running with activity");
        }
    }

    #[test]
    fn test_stage_started_stores_allowed_tools() {
        let mut state = new_state();
        state.handle_pipeline_event(PipelineEvent::StageStarted {
            stage_index: 0,
            stage_id: "s0".into(),
            stage_name: "S0".into(),
            model: "m".into(),
            runtime: "copilot-cli".into(),
            allowed_tools: vec!["read".into(), "write".into()],
            retry_group: None,
            isolated: false,
        });
        assert_eq!(state.stages[0].runtime, "copilot-cli");
        assert_eq!(state.stages[0].allowed_tools, vec!["read", "write"]);
    }

    #[test]
    fn test_p_key_toggles_prompt_view() {
        let mut state = new_state();
        state.finished = true;
        assert!(!state.show_prompt);
        state.handle_key(make_key(KeyCode::Char('p')));
        assert!(state.show_prompt);
        state.handle_key(make_key(KeyCode::Char('p')));
        assert!(!state.show_prompt);
    }

    #[test]
    fn test_help_key_returns_toggle_help() {
        let mut state = new_state();
        state.finished = true;
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Char('?'))),
            ExecutionAction::ToggleHelp
        ));
    }

    #[test]
    fn test_format_runtime_short() {
        assert_eq!(format_runtime_short("claude-code"), "claude");
        assert_eq!(format_runtime_short("copilot-cli"), "copilot");
        assert_eq!(format_runtime_short(""), "claude");
        assert_eq!(format_runtime_short("custom"), "custom");
    }
}
