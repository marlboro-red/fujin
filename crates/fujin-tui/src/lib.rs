mod app;
pub mod discovery;
mod event;
pub mod screens;
pub mod widgets;

use anyhow::Result;

/// Launch the full-screen TUI application.
///
/// This sets up the terminal (alternate screen, raw mode), runs the app
/// event loop, and restores the terminal on exit (including panics).
pub async fn run_tui() -> Result<()> {
    // Install a panic hook that restores the terminal before printing
    // the panic message — prevents garbled output on crash.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        ratatui::restore();
        original_hook(panic_info);
    }));

    let mut terminal = ratatui::init();
    let mut app = app::App::new();

    let result = app.run(&mut terminal).await;

    ratatui::restore();

    result
}
