use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

/// Render a centered help overlay popup.
pub fn render_help_overlay(frame: &mut Frame, area: Rect, is_execution: bool) {
    // Center a box
    let popup_area = centered_rect(50, 60, area);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ")
        .style(Style::default().fg(Color::White));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let mut lines = vec![
        Line::from(Span::styled(
            "  Keyboard Shortcuts",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    if is_execution {
        lines.extend([
            help_line("Ctrl+C", "Stop running pipeline"),
            help_line("j / ↓", "Next stage (after completion)"),
            help_line("k / ↑", "Previous stage (after completion)"),
            help_line("PgUp", "Scroll log up"),
            help_line("PgDn", "Scroll log down"),
            help_line("Home", "Scroll to top"),
            help_line("End", "Resume auto-scroll"),
            help_line("b", "Back to browser (after completion)"),
            help_line("q / Esc", "Quit / Stop"),
            help_line("?", "Toggle this help"),
        ]);
    } else {
        lines.extend([
            help_line("j / ↓", "Move selection down"),
            help_line("k / ↑", "Move selection up"),
            help_line("Enter", "Run selected pipeline"),
            help_line("r", "Refresh pipeline list"),
            help_line("q / Esc", "Quit"),
            help_line("?", "Toggle this help"),
        ]);
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press any key to close",
        Style::default().fg(Color::DarkGray),
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
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::raw(desc),
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
