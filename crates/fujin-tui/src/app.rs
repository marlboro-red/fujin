use crate::discovery::{discover_pipelines, DiscoveredPipeline};
use crate::event::TerminalEventReader;
use crate::screens::browser::{BrowserAction, BrowserState};
use crate::screens::execution::{ExecutionAction, ExecutionState};
use crate::screens::variables::{VariableInputAction, VariableInputState};
use crate::widgets::help::render_help_overlay;
use anyhow::Result;
use crossterm::event::{Event as CrosstermEvent, KeyEventKind};
use fujin_core::event::PipelineEvent;
use fujin_core::{PipelineRunner, RunOptions};
use ratatui::Frame;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Unified event type for the app event loop.
pub enum AppEvent {
    /// Terminal input event.
    Terminal(CrosstermEvent),
    /// Pipeline progress event.
    Pipeline(PipelineEvent),
    /// Periodic tick for UI refresh.
    Tick,
}

/// Which screen is currently displayed.
enum Screen {
    Browser(BrowserState),
    VariableInput(VariableInputState),
    Execution(ExecutionState),
}

/// Main application state.
pub struct App {
    screen: Screen,
    show_help: bool,
    should_quit: bool,
    /// Channel for receiving pipeline events during execution.
    pipeline_rx: Option<mpsc::UnboundedReceiver<PipelineEvent>>,
    /// Flag to signal pipeline cancellation.
    cancel_flag: Option<Arc<AtomicBool>>,
}

impl App {
    /// Create a new app starting at the browser screen.
    pub fn new() -> Self {
        let pipelines = discover_pipelines();
        Self {
            screen: Screen::Browser(BrowserState::new(pipelines)),
            show_help: false,
            should_quit: false,
            pipeline_rx: None,
            cancel_flag: None,
        }
    }

    /// Run the main event loop.
    pub async fn run(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        let mut event_reader = TerminalEventReader::new();
        let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(100));

        while !self.should_quit {
            // Draw
            terminal.draw(|frame| self.render(frame))?;

            // Collect events
            let events = self.collect_events(&mut event_reader, &mut tick_interval).await;

            for event in events {
                self.handle_event(event).await?;
            }
        }

        Ok(())
    }

    /// Collect all pending events (non-blocking after first tick).
    async fn collect_events(
        &mut self,
        reader: &mut TerminalEventReader,
        tick_interval: &mut tokio::time::Interval,
    ) -> Vec<AppEvent> {
        let mut events = Vec::new();

        // Wait for at least one tick
        tick_interval.tick().await;
        events.push(AppEvent::Tick);

        // Drain terminal events
        while let Some(ev) = reader.try_recv() {
            events.push(AppEvent::Terminal(ev));
        }

        // Drain pipeline events
        if let Some(ref mut rx) = self.pipeline_rx {
            while let Ok(ev) = rx.try_recv() {
                events.push(AppEvent::Pipeline(ev));
            }
        }

        events
    }

    /// Handle a single app event.
    async fn handle_event(&mut self, event: AppEvent) -> Result<()> {
        match event {
            AppEvent::Tick => {} // Just triggers a redraw

            AppEvent::Terminal(CrosstermEvent::Key(key))
                if key.kind == KeyEventKind::Press =>
            {
                // Help overlay intercepts all keys
                if self.show_help {
                    self.show_help = false;
                    return Ok(());
                }

                match &mut self.screen {
                    Screen::Browser(state) => {
                        let action = state.handle_key(key);
                        self.handle_browser_action(action).await?;
                    }
                    Screen::VariableInput(state) => {
                        let action = state.handle_key(key);
                        self.handle_variable_input_action(action)?;
                    }
                    Screen::Execution(state) => {
                        let action = state.handle_key(key);
                        self.handle_execution_action(action);
                    }
                }
            }

            AppEvent::Terminal(CrosstermEvent::Resize(_, _)) => {
                // Terminal resize — ratatui handles this on next draw
            }

            AppEvent::Terminal(_) => {} // Mouse, focus, paste events — ignore

            AppEvent::Pipeline(pipeline_event) => {
                if let Screen::Execution(ref mut state) = self.screen {
                    state.handle_pipeline_event(pipeline_event);
                }
            }
        }

        Ok(())
    }

    async fn handle_browser_action(&mut self, action: BrowserAction) -> Result<()> {
        match action {
            BrowserAction::None => {}
            BrowserAction::Quit => self.should_quit = true,
            BrowserAction::ToggleHelp => self.show_help = !self.show_help,
            BrowserAction::Refresh => {
                let pipelines = discover_pipelines();
                if let Screen::Browser(ref mut state) = self.screen {
                    state.set_pipelines(pipelines);
                }
            }
            BrowserAction::RunPipeline(idx) => {
                if let Screen::Browser(ref state) = self.screen {
                    if let Some(pipeline) = state.pipelines.get(idx) {
                        let pipeline = pipeline.clone();
                        if pipeline.config.variables.is_empty() {
                            // No variables — run immediately
                            self.start_pipeline(pipeline, std::collections::HashMap::new())?;
                        } else {
                            // Show variable input screen
                            self.screen =
                                Screen::VariableInput(VariableInputState::new(pipeline));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_variable_input_action(&mut self, action: VariableInputAction) -> Result<()> {
        match action {
            VariableInputAction::None => {}
            VariableInputAction::Cancel => {
                let pipelines = discover_pipelines();
                self.screen = Screen::Browser(BrowserState::new(pipelines));
            }
            VariableInputAction::ToggleHelp => self.show_help = !self.show_help,
            VariableInputAction::Confirm => {
                // Extract state before transitioning
                if let Screen::VariableInput(state) = &self.screen {
                    let overrides = state.overrides();
                    let pipeline = state.pipeline.clone();
                    self.start_pipeline(pipeline, overrides)?;
                }
            }
        }
        Ok(())
    }

    fn handle_execution_action(&mut self, action: ExecutionAction) {
        match action {
            ExecutionAction::None => {}
            ExecutionAction::Quit => self.should_quit = true,
            ExecutionAction::ToggleHelp => self.show_help = !self.show_help,
            ExecutionAction::BackToBrowser => {
                self.pipeline_rx = None;
                self.cancel_flag = None;
                let pipelines = discover_pipelines();
                self.screen = Screen::Browser(BrowserState::new(pipelines));
            }
            ExecutionAction::CancelPipeline => {
                if let Some(ref flag) = self.cancel_flag {
                    flag.store(true, Ordering::Relaxed);
                }
            }
        }
    }

    /// Start executing a pipeline with optional variable overrides.
    fn start_pipeline(
        &mut self,
        pipeline: DiscoveredPipeline,
        overrides: std::collections::HashMap<String, String>,
    ) -> Result<()> {
        let mut config = pipeline.config.clone();
        config.apply_overrides(&overrides);
        let raw_yaml = pipeline.raw_yaml.clone();
        let total_stages = config.stages.len();

        // Create event channel
        let (tx, rx) = mpsc::unbounded_channel();
        self.pipeline_rx = Some(rx);

        // Create cancel flag
        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.cancel_flag = Some(cancel_flag.clone());

        // Use the directory where fujin was launched so models run there.
        let workspace_root = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."));

        // Create execution screen
        let exec_state = ExecutionState::new(
            config.name.clone(),
            String::new(), // run_id will be populated via event
            total_stages,
        );
        self.screen = Screen::Execution(exec_state);

        // Spawn pipeline on a background task
        tokio::spawn(async move {
            let runner = PipelineRunner::new(config, raw_yaml, workspace_root, tx)
                .with_cancel_flag(cancel_flag);

            let options = RunOptions {
                resume: false,
                dry_run: false,
            };

            // Errors are reported via PipelineEvent::PipelineFailed/PipelineCancelled
            let _ = runner.run(&options).await;
        });

        Ok(())
    }

    /// Render the current screen.
    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        match &mut self.screen {
            Screen::Browser(state) => state.render(frame, area),
            Screen::VariableInput(state) => state.render(frame, area),
            Screen::Execution(state) => state.render(frame, area),
        }

        if self.show_help {
            let is_execution = matches!(self.screen, Screen::Execution(_));
            render_help_overlay(frame, area, is_execution);
        }
    }
}
