use crate::discovery::DiscoveredPipeline;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

/// State for the variable input screen shown before pipeline execution.
pub struct VariableInputState {
    /// The pipeline being configured.
    pub pipeline: DiscoveredPipeline,
    /// Ordered list of variable keys.
    keys: Vec<String>,
    /// Current values (same order as `keys`).
    values: Vec<String>,
    /// Which variable is currently selected.
    selected: usize,
    /// Whether the selected field is in edit mode.
    editing: bool,
    /// Cursor position within the editing value.
    cursor: usize,
}

/// Actions the variable input screen can request.
#[derive(Debug)]
pub enum VariableInputAction {
    /// No action.
    None,
    /// User confirmed — run the pipeline with these variable overrides.
    Confirm,
    /// User cancelled — go back to the browser.
    Cancel,
    /// Toggle help overlay.
    ToggleHelp,
}

impl VariableInputState {
    pub fn new(pipeline: DiscoveredPipeline) -> Self {
        let mut keys: Vec<String> = pipeline.config.variables.keys().cloned().collect();
        keys.sort();
        let values: Vec<String> = keys
            .iter()
            .map(|k| pipeline.config.variables.get(k).cloned().unwrap_or_default())
            .collect();
        Self {
            pipeline,
            keys,
            values,
            selected: 0,
            editing: false,
            cursor: 0,
        }
    }

    /// Returns true if there are no variables to edit.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Build the final variable overrides map.
    pub fn overrides(&self) -> std::collections::HashMap<String, String> {
        self.keys
            .iter()
            .zip(self.values.iter())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Handle a key event. Returns the resulting action.
    pub fn handle_key(&mut self, key: KeyEvent) -> VariableInputAction {
        if self.editing {
            return self.handle_edit_key(key);
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => VariableInputAction::Cancel,
            KeyCode::Char('?') => VariableInputAction::ToggleHelp,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                VariableInputAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.keys.len() {
                    self.selected += 1;
                }
                VariableInputAction::None
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+Enter confirms and runs
                VariableInputAction::Confirm
            }
            KeyCode::Enter | KeyCode::Char('e') => {
                // Enter edit mode for the selected variable
                if !self.keys.is_empty() {
                    self.editing = true;
                    self.cursor = self.values[self.selected].len();
                }
                VariableInputAction::None
            }
            KeyCode::Char('r') => {
                // Run pipeline (same as Ctrl+Enter)
                VariableInputAction::Confirm
            }
            _ => VariableInputAction::None,
        }
    }

    /// Handle keys while editing a field value.
    fn handle_edit_key(&mut self, key: KeyEvent) -> VariableInputAction {
        match key.code {
            KeyCode::Esc => {
                self.editing = false;
                VariableInputAction::None
            }
            KeyCode::Enter => {
                self.editing = false;
                VariableInputAction::None
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                VariableInputAction::None
            }
            KeyCode::Right => {
                let len = self.values[self.selected].len();
                if self.cursor < len {
                    self.cursor += 1;
                }
                VariableInputAction::None
            }
            KeyCode::Home => {
                self.cursor = 0;
                VariableInputAction::None
            }
            KeyCode::End => {
                self.cursor = self.values[self.selected].len();
                VariableInputAction::None
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.values[self.selected].remove(self.cursor - 1);
                    self.cursor -= 1;
                }
                VariableInputAction::None
            }
            KeyCode::Delete => {
                let len = self.values[self.selected].len();
                if self.cursor < len {
                    self.values[self.selected].remove(self.cursor);
                }
                VariableInputAction::None
            }
            KeyCode::Char(c) => {
                self.values[self.selected].insert(self.cursor, c);
                self.cursor += 1;
                VariableInputAction::None
            }
            _ => VariableInputAction::None,
        }
    }

    /// Render the variable input screen.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
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
            Span::styled(
                "  fujin",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("Configure Variables", Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(
                &self.pipeline.config.name,
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(header), area);
    }

    fn render_body(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Variables ");

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.keys.is_empty() {
            let msg = Paragraph::new("  No variables defined in this pipeline.");
            frame.render_widget(msg, inner);
            return;
        }

        let mut lines = Vec::new();
        lines.push(Line::from(""));

        for (i, (key, value)) in self.keys.iter().zip(self.values.iter()).enumerate() {
            let is_selected = i == self.selected;

            let indicator = if is_selected { "> " } else { "  " };
            let key_style = if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(indicator, Style::default().fg(Color::Cyan)),
                Span::styled(key, key_style),
            ]));

            // Value line with edit indicator
            if is_selected && self.editing {
                // Show value with cursor
                let (before, after) = value.split_at(self.cursor.min(value.len()));
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(before, Style::default().fg(Color::White)),
                    Span::styled(
                        "│",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(after, Style::default().fg(Color::White)),
                ]));
            } else {
                let val_style = if is_selected {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(value, val_style),
                ]));
            }

            lines.push(Line::from(""));
        }

        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, inner);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let footer = if self.editing {
            Line::from(vec![
                Span::styled("  [Enter]", Style::default().fg(Color::Cyan)),
                Span::raw(" Done editing  "),
                Span::styled("[Esc]", Style::default().fg(Color::Cyan)),
                Span::raw(" Cancel edit"),
            ])
        } else {
            Line::from(vec![
                Span::styled("  [r]", Style::default().fg(Color::Cyan)),
                Span::raw(" Run  "),
                Span::styled("[Enter/e]", Style::default().fg(Color::Cyan)),
                Span::raw(" Edit  "),
                Span::styled("[j/k]", Style::default().fg(Color::Cyan)),
                Span::raw(" Navigate  "),
                Span::styled("[q]", Style::default().fg(Color::Cyan)),
                Span::raw(" Back  "),
                Span::styled("[?]", Style::default().fg(Color::Cyan)),
                Span::raw(" Help"),
            ])
        };
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

    fn make_pipeline_with_vars() -> DiscoveredPipeline {
        let yaml = r#"
name: "Test Pipeline"
variables:
  target: "debug"
  arch: "x86_64"
stages:
  - id: s1
    name: Stage
    system_prompt: x
    user_prompt: y
"#;
        DiscoveredPipeline {
            source: std::path::PathBuf::from("/tmp/test.yaml"),
            config: fujin_config::PipelineConfig::from_yaml(yaml).unwrap(),
            raw_yaml: yaml.to_string(),
        }
    }

    fn make_pipeline_no_vars() -> DiscoveredPipeline {
        let yaml = r#"
name: "No Vars"
stages:
  - id: s1
    name: Stage
    system_prompt: x
    user_prompt: y
"#;
        DiscoveredPipeline {
            source: std::path::PathBuf::from("/tmp/test.yaml"),
            config: fujin_config::PipelineConfig::from_yaml(yaml).unwrap(),
            raw_yaml: yaml.to_string(),
        }
    }

    #[test]
    fn test_empty_vars() {
        let state = VariableInputState::new(make_pipeline_no_vars());
        assert!(state.is_empty());
    }

    #[test]
    fn test_has_vars() {
        let state = VariableInputState::new(make_pipeline_with_vars());
        assert!(!state.is_empty());
        assert_eq!(state.keys.len(), 2);
    }

    #[test]
    fn test_navigation() {
        let mut state = VariableInputState::new(make_pipeline_with_vars());
        assert_eq!(state.selected, 0);

        state.handle_key(make_key(KeyCode::Down));
        assert_eq!(state.selected, 1);

        // Can't go past end
        state.handle_key(make_key(KeyCode::Down));
        assert_eq!(state.selected, 1);

        state.handle_key(make_key(KeyCode::Up));
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn test_enter_edit_mode() {
        let mut state = VariableInputState::new(make_pipeline_with_vars());
        assert!(!state.editing);

        state.handle_key(make_key(KeyCode::Enter));
        assert!(state.editing);

        // Exit with Esc
        state.handle_key(make_key(KeyCode::Esc));
        assert!(!state.editing);
    }

    #[test]
    fn test_edit_value() {
        let mut state = VariableInputState::new(make_pipeline_with_vars());
        // Keys are sorted: "arch", "target"
        state.handle_key(make_key(KeyCode::Enter)); // edit first field

        // Clear and type new value
        // First, select all and delete
        for _ in 0..20 {
            state.handle_key(make_key(KeyCode::Backspace));
        }
        state.handle_key(make_key(KeyCode::Char('a')));
        state.handle_key(make_key(KeyCode::Char('r')));
        state.handle_key(make_key(KeyCode::Char('m')));
        state.handle_key(make_key(KeyCode::Enter)); // done editing

        let overrides = state.overrides();
        assert_eq!(overrides.get("arch").unwrap(), "arm");
    }

    #[test]
    fn test_cancel_returns_to_browser() {
        let mut state = VariableInputState::new(make_pipeline_with_vars());
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Char('q'))),
            VariableInputAction::Cancel
        ));
    }

    #[test]
    fn test_r_confirms() {
        let mut state = VariableInputState::new(make_pipeline_with_vars());
        assert!(matches!(
            state.handle_key(make_key(KeyCode::Char('r'))),
            VariableInputAction::Confirm
        ));
    }
}
