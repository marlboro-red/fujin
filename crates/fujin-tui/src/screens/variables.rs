use crate::discovery::DiscoveredPipeline;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
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
                    self.cursor = self.values[self.selected].chars().count();
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

    /// Convert a character-based cursor position to a byte offset in the string.
    /// Returns the byte offset of the nth character, or the string length if
    /// the cursor is at the end.
    fn cursor_to_byte_offset(s: &str, char_pos: usize) -> usize {
        s.char_indices()
            .nth(char_pos)
            .map(|(byte_idx, _)| byte_idx)
            .unwrap_or(s.len())
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
                let char_len = self.values[self.selected].chars().count();
                if self.cursor < char_len {
                    self.cursor += 1;
                }
                VariableInputAction::None
            }
            KeyCode::Home => {
                self.cursor = 0;
                VariableInputAction::None
            }
            KeyCode::End => {
                self.cursor = self.values[self.selected].chars().count();
                VariableInputAction::None
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let byte_offset = Self::cursor_to_byte_offset(
                        &self.values[self.selected],
                        self.cursor - 1,
                    );
                    self.values[self.selected].remove(byte_offset);
                    self.cursor -= 1;
                }
                VariableInputAction::None
            }
            KeyCode::Delete => {
                let char_len = self.values[self.selected].chars().count();
                if self.cursor < char_len {
                    let byte_offset = Self::cursor_to_byte_offset(
                        &self.values[self.selected],
                        self.cursor,
                    );
                    self.values[self.selected].remove(byte_offset);
                }
                VariableInputAction::None
            }
            KeyCode::Char(c) => {
                let byte_offset = Self::cursor_to_byte_offset(
                    &self.values[self.selected],
                    self.cursor,
                );
                self.values[self.selected].insert(byte_offset, c);
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
            Span::styled(
                "Configure Variables",
                Style::default().fg(theme::TEXT_SECONDARY),
            ),
            Span::raw("  "),
            Span::styled(
                &self.pipeline.config.name,
                Style::default()
                    .fg(theme::TEXT_PRIMARY)
                    .add_modifier(Modifier::BOLD),
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

    fn render_body(&self, frame: &mut Frame, area: Rect) {
        let block = theme::styled_block("Variables", false);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.keys.is_empty() {
            let msg = Paragraph::new(Span::styled(
                "No variables defined in this pipeline.",
                Style::default().fg(theme::TEXT_SECONDARY),
            ));
            frame.render_widget(msg, inner);
            return;
        }

        let mut lines = Vec::new();
        lines.push(Line::from(""));

        for (i, (key, value)) in self.keys.iter().zip(self.values.iter()).enumerate() {
            let is_selected = i == self.selected;

            let indicator = if is_selected { "> " } else { "  " };
            let bg = if is_selected {
                theme::SURFACE_HIGHLIGHT
            } else {
                ratatui::style::Color::Reset
            };
            let key_style = if is_selected {
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD)
                    .bg(bg)
            } else {
                Style::default().fg(theme::TEXT_SECONDARY).bg(bg)
            };

            lines.push(Line::from(vec![
                Span::styled(indicator, Style::default().fg(theme::ACCENT).bg(bg)),
                Span::styled(key, key_style),
            ]));

            // Value line with edit indicator
            if is_selected && self.editing {
                // Show value with cursor (use char-based offset for UTF-8 safety)
                let byte_offset = Self::cursor_to_byte_offset(value, self.cursor);
                let (before, after) = value.split_at(byte_offset);
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(before, Style::default().fg(theme::TEXT_PRIMARY)),
                    Span::styled(
                        "\u{2502}",
                        Style::default()
                            .fg(theme::WARNING)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(after, Style::default().fg(theme::TEXT_PRIMARY)),
                ]));
            } else {
                let val_style = if is_selected {
                    Style::default().fg(theme::TEXT_PRIMARY)
                } else {
                    Style::default().fg(theme::TEXT_MUTED)
                };
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(value, val_style),
                ]));
            }

            lines.push(Line::from(""));
        }

        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, inner);
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

        let footer = if self.editing {
            Line::from(vec![
                Span::styled("  [Enter]", Style::default().fg(theme::ACCENT)),
                Span::styled(" Done editing  ", Style::default().fg(theme::TEXT_SECONDARY)),
                Span::styled("[Esc]", Style::default().fg(theme::ACCENT)),
                Span::styled(" Cancel edit", Style::default().fg(theme::TEXT_SECONDARY)),
            ])
        } else {
            Line::from(vec![
                Span::styled("  [r]", Style::default().fg(theme::ACCENT)),
                Span::styled(" Run  ", Style::default().fg(theme::TEXT_SECONDARY)),
                Span::styled("[Enter/e]", Style::default().fg(theme::ACCENT)),
                Span::styled(" Edit  ", Style::default().fg(theme::TEXT_SECONDARY)),
                Span::styled("[j/k]", Style::default().fg(theme::ACCENT)),
                Span::styled(" Navigate  ", Style::default().fg(theme::TEXT_SECONDARY)),
                Span::styled("[q]", Style::default().fg(theme::ACCENT)),
                Span::styled(" Back  ", Style::default().fg(theme::TEXT_SECONDARY)),
                Span::styled("[?]", Style::default().fg(theme::ACCENT)),
                Span::styled(" Help", Style::default().fg(theme::TEXT_SECONDARY)),
            ])
        };
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
