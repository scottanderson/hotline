//! Rendering. Pure: takes `&App`, draws it, touches nothing else.

use hotline_proto::types::UserFlags;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as UiLine, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, LineKind};

pub fn draw(frame: &mut Frame, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(24)])
        .split(frame.size());

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(columns[0]);

    draw_messages(frame, rows[0], app);
    draw_input(frame, rows[1], app);
    draw_users(frame, columns[1], app);
}

fn draw_messages(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let lines: Vec<UiLine> = app
        .lines
        .iter()
        .map(|line| {
            let style = match line.kind {
                LineKind::Chat => Style::default(),
                LineKind::System => Style::default().fg(Color::DarkGray),
                LineKind::Error => Style::default().fg(Color::Red),
            };
            UiLine::from(Span::styled(line.text.clone(), style))
        })
        .collect();

    // Scroll to the bottom: only the tail that fits needs to be shown.
    let visible = area.height.saturating_sub(2) as usize;
    let start = lines.len().saturating_sub(visible);

    let paragraph = Paragraph::new(lines[start..].to_vec())
        .block(Block::default().borders(Borders::ALL).title("hotline-rs"))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_input(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let input = Paragraph::new(app.input.as_str())
        .block(Block::default().borders(Borders::ALL).title("message"));
    frame.render_widget(input, area);

    // Blinking-cursor-at-end-of-input, the usual terminal chat convention.
    frame.set_cursor(area.x + 1 + app.input.chars().count() as u16, area.y + 1);
}

fn draw_users(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let items: Vec<ListItem> = app
        .users
        .iter()
        .map(|u| {
            let label = if u.name.is_empty() {
                format!("(unnamed) #{}", u.user_id)
            } else {
                u.name.clone()
            };
            let flags = UserFlags::from_bits_truncate(u.flags);
            let mut style = Style::default();
            if flags.contains(UserFlags::ADMIN) {
                style = style.fg(Color::Red);
            } else if flags.contains(UserFlags::AWAY) {
                style = style.fg(Color::DarkGray);
            }
            if u.name == app.nick {
                style = style.add_modifier(Modifier::BOLD);
            }
            ListItem::new(Span::styled(label, style))
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("users ({})", app.users.len())),
    );
    frame.render_widget(list, area);
}
