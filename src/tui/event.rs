use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};

/// Events surfaced to the App loop.
///
/// Mouse events carry the cursor `(column, row)` so the App can route
/// scroll/click to whichever pane the cursor is over (k9s/lazygit style),
/// not just the currently-focused pane.
pub enum AppEvent {
    Key(KeyEvent),
    /// Mouse scroll up at `(col, row)`.
    Scroll(ScrollDirection, u16, u16),
    /// Left-click pressed down at `(col, row)`.
    Click(u16, u16),
    Tick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollDirection {
    Up,
    Down,
}

pub fn poll_event(tick_rate: Duration) -> Result<AppEvent> {
    if event::poll(tick_rate)? {
        match event::read()? {
            // Crossterm on linux can emit Press + Release for the same
            // physical key. Toggle-style bindings (Ctrl+E expand,
            // Ctrl+M mouse-capture) need to fire ONCE per press, not
            // twice. Drop Release/Repeat here.
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                return Ok(AppEvent::Key(key));
            }
            Event::Key(_) => {}
            Event::Mouse(MouseEvent { kind, column, row, .. }) => match kind {
                MouseEventKind::ScrollUp => {
                    return Ok(AppEvent::Scroll(ScrollDirection::Up, column, row));
                }
                MouseEventKind::ScrollDown => {
                    return Ok(AppEvent::Scroll(ScrollDirection::Down, column, row));
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    return Ok(AppEvent::Click(column, row));
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(AppEvent::Tick)
}
