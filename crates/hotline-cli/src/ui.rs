//! Rendering, and the screen areas that mouse clicks hit.

use hotline_proto::types::{FileNameWithInfo, UserFlags, UserNameWithInfo};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as UiLine, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::line::{Line, LineKind};
use crate::session::{ClientSession, Mode};
use crate::view::{contains, privileges, AccountForm, Focus, MenuItem, Modal, Tab, View};

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

/// `input` is the text in the input line, which grows to fit it. With `None`,
/// the input line has no rows.
pub fn areas(screen: Rect, input: Option<&str>) -> Areas {
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
            Constraint::Length(input.map_or(0, |text| input_rows(text) + 2)),
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

/// Each tab and its label: the fixed tabs, then one for each room.
pub fn tab_labels(session: &ClientSession) -> Vec<(Tab, String)> {
    let fixed = Tab::FIXED
        .iter()
        .map(|&(tab, label)| (tab, label.to_string()));
    let admin = session
        .can_administer()
        .then(|| (Tab::Admin, Tab::ADMIN_LABEL.to_string()));
    let rooms = session
        .rooms
        .iter()
        .map(|room| (Tab::Room(room.id), room.label()));
    fixed.chain(admin).chain(rooms).collect()
}

/// Each tab in `labels` and the columns its label covers, drawn as ` Chat │ News `.
fn tab_columns(
    tabs: Rect,
    labels: &[(Tab, String)],
) -> impl Iterator<Item = (Tab, &str, std::ops::Range<u16>)> {
    labels.iter().scan(tabs.x, |x, (tab, label)| {
        let start = *x;
        let end = start + label.chars().count() as u16 + 2;
        *x = end + 1;
        Some((*tab, label.as_str(), start..end))
    })
}

/// The tab in `labels` whose label is at `x`, `y`.
pub fn tab_at(tabs: Rect, x: u16, y: u16, labels: &[(Tab, String)]) -> Option<Tab> {
    if !contains(tabs, x, y) {
        return None;
    }
    tab_columns(tabs, labels).find_map(|(tab, _, columns)| columns.contains(&x).then_some(tab))
}

/// The users shown beside `tab`: a room's members, or everyone.
pub fn tab_users(session: &ClientSession, tab: Tab) -> &[UserNameWithInfo] {
    match tab {
        Tab::Room(id) => session.room(id).map_or(&[], |room| &room.users),
        _ => &session.users,
    }
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
    draw_tabs(frame, areas.tabs, view);
    match view.tab {
        Tab::Chat => draw_messages(frame, areas.body, &session.lines, "Hotline"),
        Tab::News => draw_news(frame, areas.body, session, view.news_scroll),
        Tab::Files => draw_files(frame, areas.body, session, view),
        Tab::Threaded => draw_threaded_news(frame, areas.body, session, view),
        Tab::Admin => draw_accounts(frame, areas.body, session, view),
        Tab::Room(id) => {
            if let Some(room) = session.room(id) {
                draw_messages(frame, areas.body, &room.lines, &room.label());
            }
        }
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

fn draw_tabs(frame: &mut Frame, area: Rect, view: &View) {
    let mut spans = Vec::new();
    for (tab, label, _) in tab_columns(area, &view.tabs) {
        if !spans.is_empty() {
            spans.push(Span::raw("│"));
        }
        let style = if tab == view.tab {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        spans.push(Span::styled(format!(" {label} "), style));
    }
    frame.render_widget(Paragraph::new(UiLine::from(spans)), area);
}

fn draw_messages(frame: &mut Frame, area: Rect, lines: &[Line], title: &str) {
    let lines: Vec<UiLine> = lines
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
        .block(Block::default().borders(Borders::ALL).title(title))
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

fn draw_files(frame: &mut Frame, area: Rect, session: &ClientSession, view: &mut View) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border(view.focus == Focus::Files))
        .title(format!("files /{}", session.file_path.components.join("/")));
    let Some(files) = &session.files else {
        frame.render_widget(Paragraph::new("loading files").block(block), area);
        return;
    };
    let items: Vec<ListItem> = files.iter().map(|f| ListItem::new(file_label(f))).collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let selected = files
        .len()
        .checked_sub(1)
        .map(|last| view.selected_file.min(last));
    let mut state = ListState::default()
        .with_offset(view.files_offset)
        .with_selected(selected);
    frame.render_stateful_widget(list, area, &mut state);
    view.files_offset = state.offset();
}

fn draw_threaded_news(frame: &mut Frame, area: Rect, session: &ClientSession, view: &mut View) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border(view.focus == Focus::NewsList))
        .title(format!(
            "threaded news /{}",
            session.threaded_path.components.join("/")
        ));
    let Some(items) = &session.threaded else {
        frame.render_widget(Paragraph::new("loading news").block(block), area);
        return;
    };
    let rows: Vec<ListItem> = items.iter().map(|i| ListItem::new(i.label())).collect();
    let list = List::new(rows)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let selected = items
        .len()
        .checked_sub(1)
        .map(|last| view.selected_news.min(last));
    let mut state = ListState::default()
        .with_offset(view.news_offset)
        .with_selected(selected);
    frame.render_stateful_widget(list, area, &mut state);
    view.news_offset = state.offset();
}

fn draw_accounts(frame: &mut Frame, area: Rect, session: &ClientSession, view: &mut View) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border(view.focus == Focus::Accounts))
        .title("accounts");
    let Some(accounts) = &session.accounts else {
        frame.render_widget(Paragraph::new("loading accounts").block(block), area);
        return;
    };
    let rows: Vec<ListItem> = accounts
        .iter()
        .map(|a| ListItem::new(format!("{}  ({})", a.login, a.name)))
        .collect();
    let list = List::new(rows)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let selected = accounts
        .len()
        .checked_sub(1)
        .map(|last| view.selected_account.min(last));
    let mut state = ListState::default()
        .with_offset(view.accounts_offset)
        .with_selected(selected);
    frame.render_stateful_widget(list, area, &mut state);
    view.accounts_offset = state.offset();
}

/// The rows of `form`: name, login, password, then a check box for each privilege.
fn account_form_rows(form: &AccountForm) -> Vec<String> {
    let text = [
        format!("name:     {}", form.name),
        format!("login:    {}", form.login),
        format!("password: {}", form.password_text()),
    ];
    let checks = privileges().map(|(label, flag)| {
        let mark = if form.access.contains(flag) { 'x' } else { ' ' };
        format!("[{mark}] {label}")
    });
    text.into_iter().chain(checks).collect()
}

/// A folder's name with a trailing `/`, or a file's name and size.
fn file_label(file: &FileNameWithInfo) -> String {
    if file.is_folder() {
        format!("{}/", file.name)
    } else {
        format!("{}  {}", file.name, size_text(file.size))
    }
}

/// `bytes` in B, KB, MB, or GB.
fn size_text(bytes: u32) -> String {
    const UNITS: [&str; 3] = ["KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = f64::from(bytes) / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

fn draw_input(frame: &mut Frame, area: Rect, view: &View) {
    let title = match view.tab {
        Tab::Chat | Tab::Room(_) => "message",
        Tab::News => "post",
        Tab::Files | Tab::Threaded | Tab::Admin => return,
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
        view.input().unwrap_or_default(),
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
    let users = tab_users(session, view.tab);
    let items: Vec<ListItem> = users
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
                .title(format!("users ({})", users.len())),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let selected = view
        .selected_user(users)
        .and_then(|id| users.iter().position(|u| u.user_id == id));
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
        Modal::Upload { input } => {
            let area = centered(screen, 60, 3);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(true))
                .title("upload a local file or folder");
            frame.render_widget(Clear, area);
            draw_text_box(frame, area, block, input, true);
        }
        Modal::SaveAs { name, input } => {
            let area = centered(screen, 60, 3);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(true))
                .title(format!("{name} is downloaded, save as"));
            frame.render_widget(Clear, area);
            draw_text_box(frame, area, block, input, true);
        }
        Modal::Confirm(conflict) => {
            let area = centered(screen, 60, 3);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(true))
                .title("resume?");
            frame.render_widget(Clear, area);
            frame.render_widget(Paragraph::new(conflict.question()).block(block), area);
        }
        Modal::Invite { from, .. } => {
            let area = centered(screen, 60, 3);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(true))
                .title("private chat");
            frame.render_widget(Clear, area);
            let text = format!("{from} invites you to a private chat.");
            frame.render_widget(Paragraph::new(text).block(block), area);
        }
        Modal::Subject { input, .. } => {
            let area = centered(screen, 60, 3);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(true))
                .title("chat subject");
            frame.render_widget(Clear, area);
            draw_text_box(frame, area, block, input, true);
        }
        Modal::PickUser {
            users, selected, ..
        } => {
            let area = centered(screen, 40, users.len().max(1) as u16 + 2);
            let items: Vec<ListItem> = users
                .iter()
                .map(|(_, name)| ListItem::new(&**name))
                .collect();
            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(border(true))
                        .title("invite to chat"),
                )
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_widget(Clear, area);
            frame.render_stateful_widget(list, area, &mut state);
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
        Modal::Article {
            title,
            poster,
            text,
            ..
        } => {
            let text = text.replace('\r', "\n");
            let height = text.lines().count() as u16 + 2;
            let area = centered(screen, 76, height.min(screen.height.saturating_sub(2)));
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(true))
                .title(format!("{title} ({poster})"));
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
                area,
            );
        }
        Modal::Post {
            parent_id,
            title,
            body,
            on_body,
        } => {
            let area = centered(screen, 70, input_rows(body) + 5);
            let title_area = Rect::new(area.x, area.y, area.width, 3);
            let body_area = Rect::new(
                area.x,
                area.y + 3,
                area.width,
                area.height.saturating_sub(3),
            );
            let heading = if *parent_id == 0 { "new post" } else { "reply" };
            let title_block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(!on_body))
                .title(heading);
            let body_block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(*on_body))
                .title("text");
            frame.render_widget(Clear, area);
            draw_text_box(frame, title_area, title_block, title, !on_body);
            draw_text_box(frame, body_area, body_block, body, *on_body);
        }
        Modal::Account(form) => {
            let rows = account_form_rows(form);
            let area = centered(screen, 50, (rows.len() as u16 + 2).min(screen.height));
            let title = match &form.old_login {
                Some(login) => format!("account {login}"),
                None => "new account".to_string(),
            };
            let list = List::new(rows)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(border(true))
                        .title(title),
                )
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            let mut state = ListState::default().with_selected(Some(form.row));
            frame.render_widget(Clear, area);
            frame.render_stateful_widget(list, area, &mut state);
        }
        Modal::OpenAccount { input } => {
            let area = centered(screen, 60, 3);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(true))
                .title("open the account with login");
            frame.render_widget(Clear, area);
            draw_text_box(frame, area, block, input, true);
        }
        Modal::DeleteAccount { login } => {
            let area = centered(screen, 60, 3);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(true))
                .title("delete?");
            frame.render_widget(Clear, area);
            let text = format!("Delete the account {login}?");
            frame.render_widget(Paragraph::new(text).block(block), area);
        }
        Modal::NewsName { category, input } => {
            let area = centered(screen, 60, 3);
            let what = if *category { "category" } else { "bundle" };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(true))
                .title(format!("new {what}"));
            frame.render_widget(Clear, area);
            draw_text_box(frame, area, block, input, true);
        }
        Modal::DeleteNews { name, .. } => {
            let area = centered(screen, 60, 3);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(true))
                .title("delete?");
            frame.render_widget(Clear, area);
            let text = format!("Delete {name}?");
            frame.render_widget(Paragraph::new(text).block(block), area);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_text_picks_the_largest_unit_under_1024() {
        assert_eq!(size_text(512), "512 B");
        assert_eq!(size_text(1536), "1.5 KB");
        assert_eq!(size_text(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(size_text(u32::MAX), "4.0 GB");
    }

    #[test]
    fn file_label_marks_a_folder_with_a_slash() {
        let folder = FileNameWithInfo {
            file_type: *b"fldr",
            creator: [0; 4],
            size: 3,
            name: "Games".into(),
        };
        assert_eq!(file_label(&folder), "Games/");
    }
}
