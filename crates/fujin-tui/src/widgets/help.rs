use crate::theme;
use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

/// Which screen the help overlay should show bindings for.
pub enum HelpMode {
    Browser,
    Variables,
    Execution,
}

/// Render a centered help overlay popup.
pub fn render_help_overlay(frame: &mut Frame, area: Rect, mode: HelpMode) {
    let popup_area = centered_rect(50, 60, area);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::BORDER_FOCUS))
        .title(" Help ")
        .title_style(
            Style::default()
                .fg(theme::TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().fg(theme::TEXT_PRIMARY));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut lines = vec![
        Line::from(Span::styled(
            "  Keyboard Shortcuts",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    match mode {
        HelpMode::Execution => {
            lines.extend([
                help_line("j / \u{2193}", "Next stage"),
                help_line("k / \u{2191}", "Previous stage"),
                help_line("G", "Jump to last stage"),
                help_line("g g", "Jump to first stage"),
                help_line("Tab", "Toggle detail panel focus"),
                help_line("f / Enter", "Toggle detail focus"),
                help_line("p", "Toggle prompt/context view"),
                help_line("PgUp", "Scroll log up"),
                help_line("PgDn", "Scroll log down"),
                help_line("Home", "Scroll to top"),
                help_line("End", "Resume auto-scroll"),
                help_line("b", "Back to browser (when finished)"),
                help_line("q", "Cancel (confirm required) / Quit"),
                help_line("Ctrl+C", "Force stop pipeline"),
                help_line("?", "Toggle this help"),
            ]);
        }
        HelpMode::Variables => {
            lines.extend([
                help_line("j / \u{2193}", "Next variable"),
                help_line("k / \u{2191}", "Previous variable"),
                help_line("Enter / e", "Edit selected variable"),
                help_line("r", "Run pipeline"),
                help_line("q / Esc", "Back to browser"),
                help_line("?", "Toggle this help"),
            ]);
        }
        HelpMode::Browser => {
            lines.extend([
                help_line("j / \u{2193}", "Move selection down"),
                help_line("k / \u{2191}", "Move selection up"),
                help_line("G", "Jump to last pipeline"),
                help_line("g g", "Jump to first pipeline"),
                help_line("Enter", "Run selected pipeline"),
                help_line("r", "Refresh pipeline list"),
                help_line("q / Esc", "Quit"),
                help_line("?", "Toggle this help"),
            ]);
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press any key to close",
        Style::default().fg(theme::TEXT_MUTED),
    )));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn help_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{key:>12}"),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(desc, Style::default().fg(theme::TEXT_PRIMARY)),
    ])
}

/// Calculate a centered rect within `area`.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([Constraint::Percentage(percent_y)])
        .flex(Flex::Center)
        .split(area);
    Layout::horizontal([Constraint::Percentage(percent_x)])
        .flex(Flex::Center)
        .split(vertical[0])[0]
}
