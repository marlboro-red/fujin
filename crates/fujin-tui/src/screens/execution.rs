use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fujin_core::event::PipelineEvent;
use fujin_core::artifact::ArtifactSet;
use fujin_core::stage::TokenUsage;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
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
}

/// Info for a stage displayed in the execution view.
#[derive(Debug, Clone)]
pub struct StageInfo {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub model: String,
    pub status: StageStatus,
    pub rendered_prompt: Option<String>,
    pub prior_summary: Option<String>,
    /// Whether a retry-group verification is currently running for this stage.
    pub verifying: bool,
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
    /// Which stage is selected for detail viewing (after completion).
    pub selected_stage: usize,
    /// Pipeline start time.
    pub start_time: Instant,
    /// Frozen elapsed time (set when pipeline finishes).
    pub final_elapsed: Option<Duration>,
    /// Cumulative token usage.
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
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
}

/// Actions the execution screen can request.
pub enum ExecutionAction {
    None,
    BackToBrowser,
    Quit,
    ToggleHelp,
    CancelPipeline,
}

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

impl ExecutionState {
    pub fn new(
        pipeline_name: String,
        run_id: String,
        total_stages: usize,
        stage_names: Vec<(String, String)>,
    ) -> Self {
        let stages = stage_names
            .into_iter()
            .enumerate()
            .map(|(i, (id, name))| StageInfo {
                index: i,
                id,
                name,
                model: String::new(),
                status: StageStatus::Pending,
                rendered_prompt: None,
                prior_summary: None,
                verifying: false,
            })            .collect();
        Self {
            pipeline_name,
            run_id,
            total_stages,
            stages,
            active_stage: None,
            selected_stage: 0,
            start_time: Instant::now(),
            final_elapsed: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            finished: false,
            final_message: None,
            spinner_frame: 0,
            detail_scroll: 0,
            detail_scroll_pinned: false,
            detail_content_height: 0,
            show_prompt: false,
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
            } => {
                // Ensure stages vec is large enough
                while self.stages.len() <= stage_index {
                    self.stages.push(StageInfo {
                        index: self.stages.len(),
                        id: String::new(),
                        name: String::new(),
                        model: String::new(),
                        status: StageStatus::Pending,
                        rendered_prompt: None,
                        prior_summary: None,
                        verifying: false,
                    });
                }
                let stage = &mut self.stages[stage_index];
                stage.id = stage_id;
                stage.name = stage_name;
                stage.model = model.clone();
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
                self.selected_stage = stage_index;
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
            } => {
                self.finished = true;
                self.final_elapsed = Some(self.start_time.elapsed());
                self.final_message = Some(format!(
                    "Pipeline complete in {:.1}s — {} stages executed",
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
                            "Retrying ({}/{}) — {}",
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
                let _ = &group_name;
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
                    if let StageStatus::Completed { activity_log, .. } = &mut stage.status {
                        activity_log.push(format!("Verifying retry group '{group_name}'…"));
                    }
                }
            }

            PipelineEvent::VerifyPassed { group_name, stage_index } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    stage.verifying = false;
                    if let StageStatus::Completed { activity_log, .. } = &mut stage.status {
                        activity_log.push(format!("Retry group '{group_name}' verification passed ✓"));
                    }
                }
            }

            PipelineEvent::VerifyFailed { group_name, stage_index, response } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    stage.verifying = false;
                    if let StageStatus::Completed { activity_log, .. } = &mut stage.status {
                        activity_log.push(format!("Retry group '{group_name}' verification failed: {response}"));
                    }
                }
            }

            PipelineEvent::VerifyActivity { stage_index, activity } => {
                if let Some(stage) = self.stages.get_mut(stage_index) {
                    if let StageStatus::Completed { activity_log, .. } = &mut stage.status {
                        activity_log.push(activity);
                    }
                }
            }
        }
    }

    /// Handle a key event. Returns the resulting action.
    pub fn handle_key(&mut self, key: KeyEvent) -> ExecutionAction {
        match key.code {
            // Ctrl+C cancels a running pipeline
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.finished {
                    ExecutionAction::CancelPipeline
                } else {
                    ExecutionAction::Quit
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.finished {
                    ExecutionAction::Quit
                } else {
                    // Esc/q also cancels during execution
                    ExecutionAction::CancelPipeline
                }
            }
            KeyCode::Char('b') if self.finished => ExecutionAction::BackToBrowser,
            KeyCode::Char('?') => ExecutionAction::ToggleHelp,
            KeyCode::Char('p') => {
                self.show_prompt = !self.show_prompt;
                self.detail_scroll = 0;
                self.detail_scroll_pinned = false;
                ExecutionAction::None
            }
            // Stage navigation
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected_stage > 0 {
                    self.selected_stage -= 1;
                    self.detail_scroll = 0;
                    self.detail_scroll_pinned = false;
                }
                ExecutionAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected_stage + 1 < self.stages.len() {
                    self.selected_stage += 1;
                    self.detail_scroll = 0;
                    self.detail_scroll_pinned = false;
                }
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

    /// Render the execution screen.
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // header
                Constraint::Min(0),    // main content
                Constraint::Length(1), // footer
            ])
            .split(area);

        self.render_header(frame, chunks[0]);
        self.render_body(frame, chunks[1]);
        self.render_footer(frame, chunks[2]);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let elapsed = self.final_elapsed.unwrap_or_else(|| self.start_time.elapsed());
        let elapsed_str = format_duration(elapsed);
        let run_short: String = self.run_id.chars().take(8).collect();

        let status = if self.finished { "Finished" } else { "Running" };

        let header = Line::from(vec![
            Span::styled(
                "  fujin",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  {status}: ")),
            Span::styled(
                &self.pipeline_name,
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  run: {run_short}  {elapsed_str}"),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(header), area);
    }

    fn render_body(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area);

        self.render_stage_list(frame, chunks[0]);
        self.render_stage_detail(frame, chunks[1]);
    }

    fn render_stage_list(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Stages ");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut lines: Vec<Line> = Vec::new();

        for (i, stage) in self.stages.iter().enumerate() {
            let (icon, icon_color, detail) = match &stage.status {
                StageStatus::Pending => ("·", Color::DarkGray, "pending".to_string()),
                StageStatus::Building => ("·", Color::Yellow, "building context...".to_string()),
                StageStatus::Running {
                    elapsed,
                    activity_log,
                } => {
                    let spinner = SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()];
                    let detail = if let Some(act) = activity_log.last() {
                        format!("{} · {act}", format_duration(*elapsed))
                    } else {
                        format!("{} elapsed...", format_duration(*elapsed))
                    };
                    (spinner, Color::Cyan, detail)
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
                            format!("{:.1}s · verifying · {act}", duration.as_secs_f64())
                        } else {
                            format!("{:.1}s · verifying…", duration.as_secs_f64())
                        };
                        (spinner, Color::Yellow, detail)
                    } else {
                        (
                            "✓",
                            Color::Green,
                            format!(
                                "{:.1}s · {}",
                                duration.as_secs_f64(),
                                artifacts.summary()
                            ),
                        )
                    }
                }
                StageStatus::Failed { .. } => ("✗", Color::Red, "failed".to_string()),
            };

            let is_selected = i == self.selected_stage;
            let name_style = if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };

            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(icon, Style::default().fg(icon_color)),
                Span::raw(format!(" {}. ", i + 1)),
                Span::styled(&stage.name, name_style),
            ]));
            lines.push(Line::from(vec![
                Span::raw("      "),
                Span::styled(detail, Style::default().fg(Color::DarkGray)),
            ]));
        }

        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, inner);
    }

    fn render_stage_detail(&mut self, frame: &mut Frame, area: Rect) {
        let detail_idx = self.selected_stage;

        let Some(stage) = self.stages.get(detail_idx) else {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Stage Detail ");
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let waiting = Paragraph::new("  Waiting for pipeline to start...");
            frame.render_widget(waiting, inner);
            return;
        };

        let title_suffix = if self.show_prompt { " [Prompt]" } else { "" };
        let title = format!(
            " Stage {}/{}: {}{} ",
            stage.index + 1,
            self.total_stages,
            stage.name,
            title_suffix,
        );
        let block = Block::default().borders(Borders::ALL).title(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Prompt/context view (toggled with 'p')
        if self.show_prompt {
            let mut lines: Vec<Line> = Vec::new();

            if let Some(ref summary) = stage.prior_summary {
                lines.push(Line::from(Span::styled(
                    "  Prior Summary:",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                )));
                for l in summary.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {l}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                lines.push(Line::from(""));
            }

            if let Some(ref prompt) = stage.rendered_prompt {
                lines.push(Line::from(Span::styled(
                    "  Rendered Prompt:",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                )));
                for l in prompt.lines() {
                    lines.push(Line::from(format!("  {l}")));
                }
            } else {
                lines.push(Line::from(Span::styled(
                    "  No prompt available (command stage or not yet built)",
                    Style::default().fg(Color::DarkGray),
                )));
            }

            let visible_height = inner.height as usize;
            let total_lines = lines.len();
            self.detail_content_height = total_lines;

            let scroll = if self.detail_scroll_pinned {
                let max_scroll = total_lines.saturating_sub(visible_height) as u16;
                self.detail_scroll.min(max_scroll)
            } else {
                0
            };

            let paragraph = Paragraph::new(lines).scroll((scroll, 0));
            frame.render_widget(paragraph, inner);
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
        let info_label = if stage.model == "commands" {
            "  type: commands".to_string()
        } else {
            format!("  model: {}", stage.model)
        };
        let mut info_lines = vec![Line::from(info_label)];

        match &stage.status {
            StageStatus::Running { elapsed, .. } => {
                info_lines.push(Line::from(format!(
                    "  Elapsed: {}",
                    format_duration(*elapsed)
                )));
            }
            StageStatus::Completed { duration, .. } => {
                info_lines.push(Line::from(vec![
                    Span::styled("  Completed in ", Style::default().fg(Color::Green)),
                    Span::styled(
                        format!("{:.1}s", duration.as_secs_f64()),
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
            _ => {}
        }

        frame.render_widget(Paragraph::new(info_lines), detail_chunks[0]);

        // Error detail
        if let StageStatus::Failed { error } = &stage.status {
            let error_paragraph = Paragraph::new(format!("  Error:\n  {error}"))
                .style(Style::default().fg(Color::Red))
                .wrap(ratatui::widgets::Wrap { trim: false });
            frame.render_widget(error_paragraph, detail_chunks[1]);
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
                    Style::default().fg(Color::DarkGray),
                )));
                for entry in log {
                    let style = if entry.starts_with("$ ") {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default().fg(Color::Cyan)
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
                    Style::default().fg(Color::DarkGray),
                )));
                for change in &artifacts.changes {
                    let (prefix, color) = match change.kind {
                        fujin_core::artifact::FileChangeKind::Created => ("+", Color::Green),
                        fujin_core::artifact::FileChangeKind::Modified => ("~", Color::Yellow),
                        fujin_core::artifact::FileChangeKind::Deleted => ("-", Color::Red),
                    };
                    let size_str = change.size.map(format_size).unwrap_or_default();
                    detail_lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(prefix, Style::default().fg(color)),
                        Span::raw(format!(" {}", change.path.display())),
                        Span::styled(
                            format!("  {size_str}"),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }
            }
            if let Some(usage) = token_usage {
                detail_lines.push(Line::from(""));
                detail_lines.push(Line::from(format!(
                    "  Tokens: {} in / {} out",
                    format_number(usage.input_tokens),
                    format_number(usage.output_tokens)
                )));
            }
        }

        if !detail_lines.is_empty() {
            let visible_height = detail_chunks[1].height as usize;
            let total_lines = detail_lines.len();

            // Store content height so scroll_detail_down can clamp properly.
            // This is an approximation (doesn't account for line wrapping),
            // but is good enough for scroll bounds.
            self.detail_content_height = total_lines;

            let scroll = if self.detail_scroll_pinned {
                // User has manually scrolled — respect their position
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
        }
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let elapsed = self.final_elapsed.unwrap_or_else(|| self.start_time.elapsed());

        let mut spans = vec![
            Span::raw("  "),
            Span::styled("Tokens: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(
                "{} in / {} out",
                format_number(self.total_input_tokens),
                format_number(self.total_output_tokens)
            )),
        ];

        if self.finished {
            spans.push(Span::raw("    "));
            spans.push(Span::styled("[b]", Style::default().fg(Color::Cyan)));
            spans.push(Span::raw(" Back  "));
            spans.push(Span::styled("[j/k]", Style::default().fg(Color::Cyan)));
            spans.push(Span::raw(" Browse stages  "));
            spans.push(Span::styled("[p]", Style::default().fg(Color::Cyan)));
            spans.push(Span::raw(" Prompt  "));
            spans.push(Span::styled("[PgUp/Dn]", Style::default().fg(Color::Cyan)));
            spans.push(Span::raw(" Scroll  "));
            spans.push(Span::styled("[q]", Style::default().fg(Color::Cyan)));
            spans.push(Span::raw(" Quit"));
        } else {
            spans.push(Span::raw("    "));
            spans.push(Span::styled("[j/k]", Style::default().fg(Color::Cyan)));
            spans.push(Span::raw(" Browse stages  "));
            spans.push(Span::styled("[p]", Style::default().fg(Color::Cyan)));
            spans.push(Span::raw(" Prompt  "));
            spans.push(Span::styled("[PgUp/Dn]", Style::default().fg(Color::Cyan)));
            spans.push(Span::raw(" Scroll  "));
            spans.push(Span::styled("[Ctrl+C]", Style::default().fg(Color::Yellow)));
            spans.push(Span::raw(" Stop"));
        }

        spans.push(Span::styled(
            format!("  Total: {}", format_duration(elapsed)),
            Style::default().fg(Color::DarkGray),
        ));

        let footer = Line::from(spans);
        frame.render_widget(Paragraph::new(footer), area);
    }
}

fn format_duration(d: Duration) -> String {
    let total_secs = d.as_secs();
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    if mins > 0 {
        format!("{mins}m{secs:02}s")
    } else {
        format!("{secs}s")
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
                ("s0".into(), "Stage 0".into()),
                ("s1".into(), "Stage 1".into()),
                ("s2".into(), "Stage 2".into()),
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
    fn test_q_cancels_while_running() {
        let mut state = new_state();
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Char('q'))),
            ExecutionAction::CancelPipeline
        ));
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
    fn test_jk_navigation_when_finished() {
        let mut state = new_state();
        state.finished = true;
        // Stages are already pre-populated by new_state() with 3 stages;
        // mark them as completed for the navigation test.
        for stage in &mut state.stages {
            stage.status = StageStatus::Completed {
                duration: Duration::from_secs(1),
                artifacts: ArtifactSet::default(),
                token_usage: None,
                activity_log: vec![],
            };
        }
        state.selected_stage = 0;

        state.handle_key(make_key(KeyCode::Char('j')));
        assert_eq!(state.selected_stage, 1);

        state.handle_key(make_key(KeyCode::Down));
        assert_eq!(state.selected_stage, 2);

        // Can't go past end
        state.handle_key(make_key(KeyCode::Char('j')));
        assert_eq!(state.selected_stage, 2);

        state.handle_key(make_key(KeyCode::Char('k')));
        assert_eq!(state.selected_stage, 1);

        state.handle_key(make_key(KeyCode::Up));
        assert_eq!(state.selected_stage, 0);

        // Can't go before start
        state.handle_key(make_key(KeyCode::Char('k')));
        assert_eq!(state.selected_stage, 0);
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
        });
        state.handle_pipeline_event(PipelineEvent::AgentRunning {
            stage_index: 0,
            stage_id: "s0".into(),
        });

        state.handle_pipeline_event(PipelineEvent::AgentActivity {
            stage_index: 0,
            activity: "Tool: Read — src/main.rs".into(),
        });
        state.handle_pipeline_event(PipelineEvent::AgentActivity {
            stage_index: 0,
            activity: "Tool: Write — output.txt".into(),
        });

        if let StageStatus::Running { activity_log, .. } = &state.stages[0].status {
            assert_eq!(activity_log.len(), 2);
            assert_eq!(activity_log[0], "Tool: Read — src/main.rs");
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
}
