//! Rendering, and the screen areas that mouse clicks hit.

use hotline_proto::types::UserFlags;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as UiLine, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::line::LineKind;
use crate::session::{ClientSession, Mode};
use crate::view::{contains, Focus, MenuItem, Modal, Tab, View};

/// Where each part of the main screen is drawn.
pub struct Areas {
    pub screen: Rect,
    pub tabs: Rect,
    /// The chat lines or the news.
    pub body: Rect,
    pub input: Rect,
    pub users: Rect,
    pub hints: Rect,
}

/// The most lines a text box shows before it scrolls.
const MAX_INPUT_ROWS: usize = 5;

/// The lines a text box needs to show `text`.
fn input_rows(text: &str) -> u16 {
    text.split('\n').count().min(MAX_INPUT_ROWS) as u16
}

/// `input` is the text in the input line, which grows to fit it.
pub fn areas(screen: Rect, input: &str) -> Areas {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(1),
        ])
        .split(screen);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(24)])
        .split(rows[1]);
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(input_rows(input) + 2),
        ])
        .split(columns[0]);
    Areas {
        screen,
        tabs: rows[0],
        body: left[0],
        input: left[1],
        users: columns[1],
        hints: rows[2],
    }
}

/// `rect` inside its border.
pub fn inner(rect: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(rect)
}

/// Each tab and the columns its label covers, drawn as ` Chat │ News `.
fn tab_columns(tabs: Rect) -> impl Iterator<Item = (Tab, std::ops::Range<u16>)> {
    Tab::ALL.into_iter().scan(tabs.x, |x, tab| {
        let start = *x;
        let end = start + tab.label().len() as u16 + 2;
        *x = end + 1;
        Some((tab, start..end))
    })
}

/// The tab whose label is at `x`, `y`.
pub fn tab_at(tabs: Rect, x: u16, y: u16) -> Option<Tab> {
    if !contains(tabs, x, y) {
        return None;
    }
    tab_columns(tabs).find_map(|(tab, columns)| columns.contains(&x).then_some(tab))
}

/// The user menu opened at `at`, kept on screen.
pub fn menu_rect(at: (u16, u16), screen: Rect) -> Rect {
    let width = MenuItem::ALL
        .iter()
        .map(|item| item.label().len() as u16)
        .max()
        .unwrap_or(0)
        + 4;
    let height = MenuItem::ALL.len() as u16 + 2;
    let x = at.0.min(screen.right().saturating_sub(width));
    let y = at.1.min(screen.bottom().saturating_sub(height));
    Rect::new(x, y, width, height).intersection(screen)
}

/// The menu item at `x`, `y` in the menu drawn at `menu`.
pub fn menu_item_at(menu: Rect, x: u16, y: u16) -> Option<MenuItem> {
    let inner = inner(menu);
    if !contains(inner, x, y) {
        return None;
    }
    MenuItem::ALL.get(usize::from(y - inner.y)).copied()
}

/// A `width` by `height` box in the middle of `screen`.
fn centered(screen: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(screen.width);
    let height = height.min(screen.height);
    Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    )
}

fn border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    }
}

pub fn draw(frame: &mut Frame, session: &ClientSession, view: &mut View) {
    if let Mode::Agreement(text) = &session.mode {
        draw_agreement(frame, text);
        return;
    }

    let areas = areas(frame.size(), view.input());
    draw_tabs(frame, areas.tabs, view.tab);
    match view.tab {
        Tab::Chat => draw_messages(frame, areas.body, session),
        Tab::News => draw_news(frame, areas.body, session, view.news_scroll),
    }
    draw_input(frame, areas.input, view);
    draw_users(frame, areas.users, session, view);
    draw_hints(frame, areas.hints, view);
    if let Some(modal) = &view.modal {
        draw_modal(frame, areas.screen, modal);
    }
}

fn draw_agreement(frame: &mut Frame, text: &str) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(frame.size());

    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title("agreement"))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, rows[0]);

    let hint = Paragraph::new("press Enter to accept, Esc to quit")
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(hint, rows[1]);
}

fn draw_tabs(frame: &mut Frame, area: Rect, current: Tab) {
    let mut spans = Vec::new();
    for (tab, _) in tab_columns(area) {
        if !spans.is_empty() {
            spans.push(Span::raw("│"));
        }
        let style = if tab == current {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        spans.push(Span::styled(format!(" {} ", tab.label()), style));
    }
    frame.render_widget(Paragraph::new(UiLine::from(spans)), area);
}

fn draw_messages(frame: &mut Frame, area: Rect, session: &ClientSession) {
    let lines: Vec<UiLine> = session
        .lines
        .iter()
        .map(|line| {
            let style = match line.kind {
                LineKind::Chat => Style::default(),
                LineKind::System => Style::default().fg(Color::DarkGray),
                LineKind::Error => Style::default().fg(Color::Red),
            };
            UiLine::from(Span::styled(&*line.text, style))
        })
        .collect();

    // Scroll to the bottom: only the tail that fits needs to be shown.
    let visible = area.height.saturating_sub(2) as usize;
    let start = lines.len().saturating_sub(visible);

    let paragraph = Paragraph::new(lines[start..].to_vec())
        .block(Block::default().borders(Borders::ALL).title("Hotline"))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_news(frame: &mut Frame, area: Rect, session: &ClientSession, scroll: u16) {
    let text = match &session.news {
        Some(news) => news.replace('\r', "\n"),
        None => "loading news".to_string(),
    };
    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title("news"))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

fn draw_input(frame: &mut Frame, area: Rect, view: &View) {
    let title = match view.tab {
        Tab::Chat => "message",
        Tab::News => "post",
    };
    let focused = view.focus == Focus::Input;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border(focused))
        .title(title);
    draw_text_box(
        frame,
        area,
        block,
        view.input(),
        focused && view.modal.is_none(),
    );
}

/// Draw `text` scrolled to its last line, with the cursor at its end if `cursor`.
fn draw_text_box(frame: &mut Frame, area: Rect, block: Block, text: &str, cursor: bool) {
    let lines = text.split('\n').count() as u16;
    let rows = input_rows(text);
    let paragraph = Paragraph::new(text).block(block).scroll((lines - rows, 0));
    frame.render_widget(paragraph, area);

    if cursor {
        let last = text.rsplit('\n').next().unwrap_or_default();
        let x = area.x + 1 + last.chars().count() as u16;
        frame.set_cursor(x.min(area.right().saturating_sub(2)), area.y + rows);
    }
}

fn draw_users(frame: &mut Frame, area: Rect, session: &ClientSession, view: &mut View) {
    let items: Vec<ListItem> = session
        .users
        .iter()
        .map(|u| {
            let label = if u.name.is_empty() {
                format!("(unnamed) #{}", u.user_id)
            } else {
                u.name.to_string()
            };
            let flags = u.flags;
            let mut style = Style::default();
            if flags.contains(UserFlags::ADMIN) {
                style = style.fg(Color::Red);
            } else if flags.contains(UserFlags::AWAY) {
                style = style.fg(Color::DarkGray);
            }
            if u.name == session.nick {
                style = style.add_modifier(Modifier::BOLD);
            }
            ListItem::new(Span::styled(label, style))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border(view.focus == Focus::Users))
                .title(format!("users ({})", session.users.len())),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let selected = view
        .selected_user(&session.users)
        .and_then(|id| session.users.iter().position(|u| u.user_id == id));
    let mut state = ListState::default()
        .with_offset(view.users_offset)
        .with_selected(selected);
    frame.render_stateful_widget(list, area, &mut state);
    view.users_offset = state.offset();
}

fn draw_hints(frame: &mut Frame, area: Rect, view: &View) {
    let mut spans = Vec::new();
    for (key, action) in view.hints() {
        spans.push(Span::styled(
            *key,
            Style::default().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(format!(" {action}  ")));
    }
    frame.render_widget(Paragraph::new(UiLine::from(spans)), area);
}

fn draw_modal(frame: &mut Frame, screen: Rect, modal: &Modal) {
    match modal {
        Modal::InstantMsg { name, input, .. } => {
            let area = centered(screen, 60, input_rows(input) + 2);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(true))
                .title(format!("message to {name}"));
            frame.render_widget(Clear, area);
            draw_text_box(frame, area, block, input, true);
        }
        Modal::UserMenu { at, selected, .. } => {
            let area = menu_rect(*at, screen);
            let items: Vec<ListItem> = MenuItem::ALL
                .iter()
                .map(|item| ListItem::new(item.label()))
                .collect();
            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(border(true)),
                )
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_widget(Clear, area);
            frame.render_stateful_widget(list, area, &mut state);
        }
        Modal::Info { name, text } => {
            let text = text.replace('\r', "\n");
            let height = text.lines().count() as u16 + 2;
            let area = centered(screen, 70, height);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(true))
                .title(format!("info for {name}"));
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
                area,
            );
        }
    }
}
