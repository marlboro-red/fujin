use crate::discovery::DiscoveredPipeline;
use crossterm::event::{KeyCode, KeyEvent};
use fujin_core::util::truncate_chars;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};

/// State for the pipeline browser screen.
pub struct BrowserState {
    /// Discovered pipelines.
    pub pipelines: Vec<DiscoveredPipeline>,
    /// Selection state for the list widget.
    pub list_state: ListState,
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
                BrowserAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
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
        let header = Line::from(vec![
            Span::styled("  fujin", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(
                "Pipeline Browser",
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

        self.render_pipeline_list(frame, chunks[0]);
        self.render_details(frame, chunks[1]);
    }

    fn render_pipeline_list(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .pipelines
            .iter()
            .map(|p| {
                let stage_count = p.config.stages.len();
                let label = format!("  {}  {}s", p.config.name, stage_count);
                ListItem::new(Line::from(label))
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Pipelines "),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");

        frame.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_details(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Details ");

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(pipeline) = self.selected() else {
            let configs_dir = fujin_core::paths::configs_dir();
            let empty = Paragraph::new(format!(
                "  No pipelines found.\n\n  Place .yaml configs in:\n    {}\n\n  Or run `fujin setup` to initialize.",
                configs_dir.display()
            ));
            frame.render_widget(empty, inner);
            return;
        };

        let config = &pipeline.config;
        let mut lines = vec![
            Line::from(vec![
                Span::styled("  Name: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    &config.name,
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Source: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{}", pipeline.source.display())),
            ]),
            Line::from(vec![
                Span::styled("  Stages: ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{}", config.stages.len())),
            ]),
            Line::from(""),
        ];

        // Stage details
        for (i, stage) in config.stages.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {}. ", i + 1),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(&stage.name, Style::default().add_modifier(Modifier::BOLD)),
            ]));
            lines.push(Line::from(format!(
                "     model: {}",
                stage.model
            )));
            if !stage.allowed_tools.is_empty() {
                lines.push(Line::from(format!(
                    "     tools: {}",
                    stage.allowed_tools.join(", ")
                )));
            }
            lines.push(Line::from(""));
        }

        // Variables
        if !config.variables.is_empty() {
            lines.push(Line::from(Span::styled(
                "  Variables:",
                Style::default().fg(Color::DarkGray),
            )));
            for (key, value) in &config.variables {
                let display_val = truncate_chars(value, 40);
                lines.push(Line::from(format!(
                    "    {key} = \"{display_val}\""
                )));
            }
        }

        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, inner);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let footer = Line::from(vec![
            Span::styled("  [Enter]", Style::default().fg(Color::Cyan)),
            Span::raw(" Run  "),
            Span::styled("[r]", Style::default().fg(Color::Cyan)),
            Span::raw(" Refresh  "),
            Span::styled("[q]", Style::default().fg(Color::Cyan)),
            Span::raw(" Quit  "),
            Span::styled("[?]", Style::default().fg(Color::Cyan)),
            Span::raw(" Help"),
        ]);
        frame.render_widget(Paragraph::new(footer), area);
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
}
