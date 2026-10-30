//! Toolbar: one row of chips, one per panel, in a pane of its own across the bottom of the tab.
//!   sidekick toolbar              the `toolbar` pane

use std::time::Duration;

use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::tui::{self, Screen, Toolbar};

/// sidekick.sh finds the pane by this label.
pub const LABEL: &str = "▤ sidekick";

const HINT: &str = "click to toggle  q hide  Q hide all ";

#[derive(Default)]
struct Bar {
    chips: Toolbar,
}

impl Screen for Bar {
    fn tick(&mut self) {}

    fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        let buf = f.buffer_mut();
        let used = self.chips.render(buf, area);
        let hint_w = HINT.width() as u16;
        if used.height == 1 && used.right() + hint_w < area.width {
            let x = area.width - hint_w;
            buf.set_line(x, used.y, &Line::from(HINT.dark_gray()), hint_w);
        } else if used.bottom() + 1 < area.height {
            // Narrow and stacked: the hint goes under the chips.
            let short = tui::truncate(" click to toggle · q hide", area.width as usize);
            buf.set_line(0, used.bottom() + 1, &Line::from(short.dark_gray()), area.width);
        }
    }

    fn event(&mut self, e: Event) -> bool {
        match e {
            Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Char('q') => {
                    tui::close_here("toolbar");
                    return false;
                }
                KeyCode::Char('Q') => {
                    tui::hide_everywhere(LABEL, "toolbar");
                    return false;
                }
                _ => {}
            },
            Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                self.chips.click(m.column, m.row);
            }
            _ => {}
        }
        true
    }
}

pub fn main(mut args: Vec<String>) -> std::io::Result<()> {
    let snapshot = tui::take_snapshot_arg(&mut args);
    let mut bar = Bar::default();
    if snapshot.is_none() {
        tui::herdr(&["pane", "rename", "$PANE", LABEL]);
        tui::resync_size();
    }
    match snapshot {
        Some(size) => tui::print_snapshot(&mut bar, &size, (100, 1)),
        None => tui::run(&mut bar, Duration::from_millis(250)),
    }
}
