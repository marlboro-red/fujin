use crate::discovery::DiscoveredPipeline;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use fujin_core::util::truncate_chars;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

/// State for the pipeline browser screen.
pub struct BrowserState {
    /// Discovered pipelines.
    pub pipelines: Vec<DiscoveredPipeline>,
    /// Selection state for the list widget.
    pub list_state: ListState,
    /// Scroll offset for the details panel.
    pub detail_scroll: u16,
    /// Total content height of the details panel.
    detail_content_height: usize,
}

/// Actions the browser can request from the app.
#[derive(Debug)]
pub enum BrowserAction {
    /// No action.
    None,
    /// User selected a pipeline to run.
    RunPipeline(usize),
    /// User wants to quit.
    Quit,
    /// User wants to refresh the pipeline list.
    Refresh,
    /// Toggle help overlay.
    ToggleHelp,
}

impl BrowserState {
    pub fn new(pipelines: Vec<DiscoveredPipeline>) -> Self {
        let mut list_state = ListState::default();
        if !pipelines.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            pipelines,
            list_state,
            detail_scroll: 0,
            detail_content_height: 0,
        }
    }

    /// Replace the pipeline list (e.g., after refresh).
    pub fn set_pipelines(&mut self, pipelines: Vec<DiscoveredPipeline>) {
        self.pipelines = pipelines;
        if self.pipelines.is_empty() {
            self.list_state.select(None);
        } else {
            let idx = self.list_state.selected().unwrap_or(0);
            self.list_state
                .select(Some(idx.min(self.pipelines.len() - 1)));
        }
    }

    /// Handle a key event. Returns the resulting action.
    pub fn handle_key(&mut self, key: KeyEvent) -> BrowserAction {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => BrowserAction::Quit,
            KeyCode::Char('?') => BrowserAction::ToggleHelp,
            KeyCode::Char('r') => BrowserAction::Refresh,
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                self.detail_scroll = 0;
                BrowserAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                self.detail_scroll = 0;
                BrowserAction::None
            }
            KeyCode::Char('G') => {
                // Jump to last
                if !self.pipelines.is_empty() {
                    self.list_state.select(Some(self.pipelines.len() - 1));
                    self.detail_scroll = 0;
                }
                BrowserAction::None
            }
            KeyCode::Char('g') => {
                // Jump to first (gg emulation: single g goes to top)
                self.list_state.select(Some(0));
                self.detail_scroll = 0;
                BrowserAction::None
            }
            KeyCode::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(10);
                BrowserAction::None
            }
            KeyCode::PageDown => {
                let max = self.detail_content_height.saturating_sub(1) as u16;
                self.detail_scroll = (self.detail_scroll + 10).min(max);
                BrowserAction::None
            }
            KeyCode::Enter => {
                if let Some(idx) = self.list_state.selected() {
                    BrowserAction::RunPipeline(idx)
                } else {
                    BrowserAction::None
                }
            }
            _ => BrowserAction::None,
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.pipelines.is_empty() {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let len = self.pipelines.len() as i32;
        let next = (current + delta).clamp(0, len - 1) as usize;
        self.list_state.select(Some(next));
    }

    /// Currently selected pipeline, if any.
    pub fn selected(&self) -> Option<&DiscoveredPipeline> {
        self.list_state
            .selected()
            .and_then(|i| self.pipelines.get(i))
    }

    /// Render the browser screen.
    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // header
                Constraint::Min(0),    // main content
                Constraint::Length(2), // footer
            ])
            .split(area);

        self.render_header(frame, chunks[0]);
        self.render_body(frame, chunks[1]);
        self.render_footer(frame, chunks[2]);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let header = Line::from(vec![
            Span::styled(
                "  fujin",
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" \u{2502} ", Style::default().fg(theme::BORDER)),
            Span::styled("Pipeline Browser", Style::default().fg(theme::TEXT_SECONDARY)),
        ]);
        // Use 2-line area: line 1 = content, line 2 = separator
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

    fn render_body(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area);

        self.render_pipeline_list(frame, chunks[0]);
        self.render_details(frame, chunks[1]);
    }

    fn render_pipeline_list(&mut self, frame: &mut Frame, area: Rect) {
        let selected_idx = self.list_state.selected();
        let items: Vec<ListItem> = self
            .pipelines
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let stage_count = p.config.stages.len();
                let stage_label = if stage_count == 1 { "stage" } else { "stages" };

                // Collect unique models across stages
                let models: Vec<String> = {
                    let mut seen = std::collections::HashSet::new();
                    p.config
                        .stages
                        .iter()
                        .filter(|s| !s.model.is_empty() && s.model != "commands")
                        .filter(|s| seen.insert(s.model.clone()))
                        .map(|s| theme::shorten_model(&s.model))
                        .collect()
                };
                let model_summary = if models.is_empty() {
                    String::new()
                } else {
                    format!("  {}", models.join(", "))
                };

                let is_selected = selected_idx == Some(i);
                let bg = if is_selected {
                    theme::SURFACE_HIGHLIGHT
                } else {
                    ratatui::style::Color::Reset
                };

                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            format!("  {} ", p.config.name),
                            Style::default()
                                .add_modifier(Modifier::BOLD)
                                .bg(bg),
                        ),
                        Span::styled(
                            format!("{stage_count} {stage_label}"),
                            Style::default().fg(theme::TEXT_MUTED).bg(bg),
                        ),
                    ]),
                    Line::from(Span::styled(
                        format!("  {model_summary}"),
                        Style::default().fg(theme::TOKEN_LABEL).bg(bg),
                    )),
                ])
            })
            .collect();

        let block = theme::styled_block("Pipelines", false);

        let list = List::new(items)
            .block(block)
            .highlight_style(
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");

        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_details(&mut self, frame: &mut Frame, area: Rect) {
        let block = theme::styled_block("Details", false);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let selected_idx = self.list_state.selected();
        let Some(pipeline) = selected_idx.and_then(|i| self.pipelines.get(i)) else {
            let configs_dir = fujin_core::paths::configs_dir();
            let empty = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "No pipelines found.",
                    Style::default().fg(theme::TEXT_SECONDARY),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Place .yaml configs in:",
                    Style::default().fg(theme::TEXT_MUTED),
                )),
                Line::from(Span::styled(
                    format!("  {}", configs_dir.display()),
                    Style::default().fg(theme::TEXT_PRIMARY),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Or run `fujin setup` to initialize.",
                    Style::default().fg(theme::TEXT_MUTED),
                )),
            ]);
            frame.render_widget(empty, inner);
            return;
        };

        // Build all lines using owned strings so the pipeline borrow is released
        let config = &pipeline.config;
        let lines = {
            let mut lines: Vec<Line<'static>> = vec![
                Line::from(vec![
                    Span::styled("Name: ", Style::default().fg(theme::TEXT_MUTED)),
                    Span::styled(
                        config.name.clone(),
                        Style::default()
                            .fg(theme::TEXT_PRIMARY)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("Source: ", Style::default().fg(theme::TEXT_MUTED)),
                    Span::styled(
                        format!("{}", pipeline.source.display()),
                        Style::default().fg(theme::TEXT_SECONDARY),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("Stages: ", Style::default().fg(theme::TEXT_MUTED)),
                    Span::styled(
                        format!("{}", config.stages.len()),
                        Style::default().fg(theme::TEXT_PRIMARY),
                    ),
                ]),
                Line::from(""),
            ];

            // Stage details
            let width = config.stages.len().to_string().len();
            for (i, stage) in config.stages.iter().enumerate() {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:>width$}. ", i + 1),
                        Style::default().fg(theme::ACCENT),
                    ),
                    Span::styled(
                        stage.name.clone(),
                        Style::default()
                            .fg(theme::TEXT_PRIMARY)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                let effective_runtime = stage.runtime.as_deref().unwrap_or(&config.runtime);
                let rt_short = match effective_runtime {
                    "claude-code" | "" => "claude",
                    "copilot-cli" => "copilot",
                    other => other,
                };
                let rt_color = match effective_runtime {
                    "copilot-cli" => ratatui::style::Color::Rgb(110, 200, 250),
                    _ => theme::TOKEN_LABEL,
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:width$}  ", ""),
                        Style::default().fg(theme::TEXT_MUTED),
                    ),
                    Span::styled(
                        rt_short.to_string(),
                        Style::default().fg(rt_color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" · ", Style::default().fg(theme::TEXT_MUTED)),
                    Span::styled(
                        theme::shorten_model(&stage.model),
                        Style::default().fg(theme::TOKEN_LABEL),
                    ),
                ]));
                if !stage.allowed_tools.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("{:width$}  tools: ", ""),
                            Style::default().fg(theme::TEXT_MUTED),
                        ),
                        Span::styled(
                            stage.allowed_tools.join(", "),
                            Style::default().fg(theme::TEXT_SECONDARY),
                        ),
                    ]));
                }
                lines.push(Line::from(""));
            }

            // Variables
            if !config.variables.is_empty() {
                lines.push(Line::from(Span::styled(
                    "Variables:".to_string(),
                    Style::default().fg(theme::TEXT_MUTED),
                )));
                for (key, value) in &config.variables {
                    let display_val = truncate_chars(value, 40);
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {key}"),
                            Style::default().fg(theme::TEXT_SECONDARY),
                        ),
                        Span::styled(" = ".to_string(), Style::default().fg(theme::TEXT_MUTED)),
                        Span::styled(
                            format!("\"{display_val}\""),
                            Style::default().fg(theme::TEXT_PRIMARY),
                        ),
                    ]));
                }
            }
            lines
        };
        // Pipeline borrow is now dropped

        let total_lines = lines.len();
        self.detail_content_height = total_lines;
        let visible_height = inner.height as usize;
        let max_scroll = total_lines.saturating_sub(visible_height) as u16;
        let scroll = self.detail_scroll.min(max_scroll);

        let paragraph = Paragraph::new(lines).scroll((scroll, 0));
        frame.render_widget(paragraph, inner);

        // Scrollbar if content overflows
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

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);
        let sep = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(theme::BORDER));
        frame.render_widget(sep, sub[0]);

        let footer = Line::from(vec![
            Span::styled("  [Enter]", Style::default().fg(theme::ACCENT)),
            Span::styled(" Run  ", Style::default().fg(theme::TEXT_SECONDARY)),
            Span::styled("[r]", Style::default().fg(theme::ACCENT)),
            Span::styled(" Refresh  ", Style::default().fg(theme::TEXT_SECONDARY)),
            Span::styled("[q]", Style::default().fg(theme::ACCENT)),
            Span::styled(" Quit  ", Style::default().fg(theme::TEXT_SECONDARY)),
            Span::styled("[?]", Style::default().fg(theme::ACCENT)),
            Span::styled(" Help", Style::default().fg(theme::TEXT_SECONDARY)),
        ]);
        frame.render_widget(Paragraph::new(footer), sub[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn empty_browser() -> BrowserState {
        BrowserState::new(vec![])
    }

    fn browser_with_items(n: usize) -> BrowserState {
        let pipelines: Vec<_> = (0..n)
            .map(|i| {
                let yaml = format!(
                    "name: \"Pipeline {i}\"\nstages:\n  - id: s{i}\n    name: Stage\n    system_prompt: x\n    user_prompt: y\n"
                );
                crate::discovery::DiscoveredPipeline {
                    source: std::path::PathBuf::from(format!("/tmp/p{i}.yaml")),
                    config: fujin_config::PipelineConfig::from_yaml(&yaml).unwrap(),
                    raw_yaml: yaml,
                }
            })
            .collect();
        BrowserState::new(pipelines)
    }

    #[test]
    fn test_quit_key() {
        let mut state = empty_browser();
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Char('q'))),
            BrowserAction::Quit
        ));
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Esc)),
            BrowserAction::Quit
        ));
    }

    #[test]
    fn test_help_key() {
        let mut state = empty_browser();
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Char('?'))),
            BrowserAction::ToggleHelp
        ));
    }

    #[test]
    fn test_refresh_key() {
        let mut state = empty_browser();
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Char('r'))),
            BrowserAction::Refresh
        ));
    }

    #[test]
    fn test_navigation() {
        let mut state = browser_with_items(3);
        assert_eq!(state.list_state.selected(), Some(0));

        state.handle_key(make_key(KeyCode::Down));
        assert_eq!(state.list_state.selected(), Some(1));

        state.handle_key(make_key(KeyCode::Char('j')));
        assert_eq!(state.list_state.selected(), Some(2));

        // Can't go past end
        state.handle_key(make_key(KeyCode::Down));
        assert_eq!(state.list_state.selected(), Some(2));

        state.handle_key(make_key(KeyCode::Up));
        assert_eq!(state.list_state.selected(), Some(1));

        state.handle_key(make_key(KeyCode::Char('k')));
        assert_eq!(state.list_state.selected(), Some(0));

        // Can't go before start
        state.handle_key(make_key(KeyCode::Up));
        assert_eq!(state.list_state.selected(), Some(0));
    }

    #[test]
    fn test_enter_runs_selected() {
        let mut state = browser_with_items(3);
        state.handle_key(make_key(KeyCode::Down));
        match state.handle_key(make_key(KeyCode::Enter)) {
            BrowserAction::RunPipeline(idx) => assert_eq!(idx, 1),
            other => panic!("Expected RunPipeline, got {other:?}"),
        }
    }

    #[test]
    fn test_enter_on_empty_list() {
        let mut state = empty_browser();
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Enter)),
            BrowserAction::None
        ));
    }

    #[test]
    fn test_set_pipelines_preserves_selection() {
        let mut state = browser_with_items(5);
        state.handle_key(make_key(KeyCode::Down));
        state.handle_key(make_key(KeyCode::Down));
        assert_eq!(state.list_state.selected(), Some(2));

        // Shrink list — selection should clamp
        let smaller: Vec<_> = (0..2)
            .map(|i| {
                let yaml = format!(
                    "name: P{i}\nstages:\n  - id: s\n    name: S\n    system_prompt: x\n    user_prompt: y\n"
                );
                crate::discovery::DiscoveredPipeline {
                    source: std::path::PathBuf::from(format!("/tmp/p{i}.yaml")),
                    config: fujin_config::PipelineConfig::from_yaml(&yaml).unwrap(),
                    raw_yaml: yaml,
                }
            })
            .collect();
        state.set_pipelines(smaller);
        assert_eq!(state.list_state.selected(), Some(1)); // clamped to last
    }

    #[test]
    fn test_g_jumps_to_top() {
        let mut state = browser_with_items(5);
        state.handle_key(make_key(KeyCode::Down));
        state.handle_key(make_key(KeyCode::Down));
        assert_eq!(state.list_state.selected(), Some(2));

        state.handle_key(make_key(KeyCode::Char('g')));
        assert_eq!(state.list_state.selected(), Some(0));
    }

    #[test]
    fn test_big_g_jumps_to_bottom() {
        let mut state = browser_with_items(5);
        assert_eq!(state.list_state.selected(), Some(0));

        state.handle_key(make_key(KeyCode::Char('G')));
        assert_eq!(state.list_state.selected(), Some(4));
    }
}
