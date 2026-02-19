use ratatui::{
    style::{Color, Modifier, Style},
    widgets::{Block, BorderType, Borders, Padding},
};

// ── Color palette ──────────────────────────────────────────────────────

pub const ACCENT: Color = Color::Rgb(130, 170, 255);
pub const SUCCESS: Color = Color::Rgb(120, 220, 120);
pub const ERROR: Color = Color::Rgb(240, 100, 100);
pub const WARNING: Color = Color::Rgb(240, 200, 80);

pub const TEXT_PRIMARY: Color = Color::Rgb(220, 220, 230);
pub const TEXT_SECONDARY: Color = Color::Rgb(140, 140, 160);
pub const TEXT_MUTED: Color = Color::Rgb(90, 90, 110);

pub const BORDER: Color = Color::Rgb(60, 60, 80);
pub const BORDER_FOCUS: Color = Color::Rgb(130, 170, 255); // same as ACCENT
pub const SURFACE_HIGHLIGHT: Color = Color::Rgb(45, 45, 60);

pub const TOKEN_LABEL: Color = Color::Rgb(180, 140, 255);

// ── Helpers ────────────────────────────────────────────────────────────

/// Build a themed block with rounded borders, optional title, and focus coloring.
pub fn styled_block(title: &str, focused: bool) -> Block<'_> {
    let border_color = if focused { BORDER_FOCUS } else { BORDER };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .padding(Padding::new(1, 1, 0, 0));
    if !title.is_empty() {
        block = block.title(format!(" {title} "))
            .title_style(Style::default().fg(TEXT_PRIMARY).add_modifier(Modifier::BOLD));
    }
    block
}

/// Shorten a model identifier for display.
///
/// Examples:
///   "claude-sonnet-4-6"  -> "sonnet 4.6"
///   "claude-opus-4-6"    -> "opus 4.6"
///   "claude-haiku-3-5"   -> "haiku 3.5"
///   "commands"           -> "commands"
///   anything else        -> returned as-is
pub fn shorten_model(name: &str) -> String {
    // Handle known Claude model patterns: claude-{family}-{major}-{minor}
    let stripped = name.strip_prefix("claude-").unwrap_or(name);

    // Try to split off a version suffix like "-4-6" or "-3-5"
    // Walk from the end looking for the pattern: -{digit}-{digit}
    let parts: Vec<&str> = stripped.rsplitn(3, '-').collect();
    if parts.len() == 3 {
        if let (Ok(minor), Ok(major)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            return format!("{} {major}.{minor}", parts[2]);
        }
    }

    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shorten_model_claude() {
        assert_eq!(shorten_model("claude-sonnet-4-6"), "sonnet 4.6");
        assert_eq!(shorten_model("claude-opus-4-6"), "opus 4.6");
        assert_eq!(shorten_model("claude-haiku-3-5"), "haiku 3.5");
    }

    #[test]
    fn test_shorten_model_passthrough() {
        assert_eq!(shorten_model("commands"), "commands");
        assert_eq!(shorten_model("gpt-4o"), "gpt-4o");
    }

    #[test]
    fn test_shorten_model_unknown_format() {
        assert_eq!(shorten_model("my-custom-model"), "my-custom-model");
    }
}
