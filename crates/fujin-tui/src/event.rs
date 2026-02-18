use crossterm::event::{self, Event as CrosstermEvent};
use std::time::Duration;
use tokio::sync::mpsc;

/// Terminal event reader that polls crossterm events and sends them
/// through a channel. Runs on a dedicated thread to avoid blocking
/// the async runtime.
pub struct TerminalEventReader {
    rx: mpsc::UnboundedReceiver<CrosstermEvent>,
}

impl TerminalEventReader {
    /// Start polling terminal events. Returns a reader that receives events.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();

        std::thread::spawn(move || {
            loop {
                if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                    if let Ok(ev) = event::read() {
                        if tx.send(ev).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Self { rx }
    }

    /// Try to receive the next terminal event without blocking.
    pub fn try_recv(&mut self) -> Option<CrosstermEvent> {
        self.rx.try_recv().ok()
    }
}
