//! Rendering, and the screen areas that mouse clicks hit.

use hotline_client::FileInfo;
use hotline_proto::types::{FileNameWithInfo, UserFlags, UserNameWithInfo};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as UiLine, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::line::{Line, LineKind};
use crate::session::{ClientSession, Mode};
use crate::view::{
    contains, privileges, AccountForm, Focus, ListCursor, MenuItem, Modal, OptionsForm, Tab, View,
    OPTIONS,
};

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

/// A key hint: the key, and what it does.
pub type Hint = (&'static str, &'static str);

/// `hints` laid out in rows no wider than `width`, keeping each hint whole.
fn hint_rows(hints: &[Hint], width: u16) -> Vec<&[Hint]> {
    let mut rows = Vec::new();
    let (mut start, mut used) = (0, 0);
    for (i, (key, action)) in hints.iter().enumerate() {
        let len = key.chars().count() + action.chars().count() + 3;
        if used > 0 && used + len > usize::from(width) {
            rows.push(&hints[start..i]);
            (start, used) = (i, 0);
        }
        used += len;
    }
    rows.push(&hints[start..]);
    rows
}

/// `input` is the text in the input line, which grows to fit it. With `None`,
/// the input line has no rows. The hint line grows to fit `hints`.
pub fn areas(screen: Rect, input: Option<&str>, hints: &[Hint]) -> Areas {
    let hint_rows = hint_rows(hints, screen.width).len() as u16;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(hint_rows),
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

/// The user menu of `items` opened at `at`, kept on screen.
pub fn menu_rect(at: (u16, u16), items: &[MenuItem], screen: Rect) -> Rect {
    let width = items
        .iter()
        .map(|item| item.label().len() as u16)
        .max()
        .unwrap_or(0)
        + 4;
    let height = items.len() as u16 + 2;
    let x = at.0.min(screen.right().saturating_sub(width));
    let y = at.1.min(screen.bottom().saturating_sub(height));
    Rect::new(x, y, width, height).intersection(screen)
}

/// The item of `items` at `x`, `y` in the menu drawn at `menu`.
pub fn menu_item_at(menu: Rect, items: &[MenuItem], x: u16, y: u16) -> Option<MenuItem> {
    let inner = inner(menu);
    if !contains(inner, x, y) {
        return None;
    }
    items.get(usize::from(y - inner.y)).copied()
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

    let areas = areas(frame.size(), view.input(), view.hints());
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
    draw_list(frame, area, block, items, &mut view.files);
}

/// Draw `items` in `block` with the selection and scroll position of `cursor`.
fn draw_list(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    items: Vec<ListItem>,
    cursor: &mut ListCursor,
) {
    let selected = items
        .len()
        .checked_sub(1)
        .map(|last| cursor.selected.min(last));
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default()
        .with_offset(cursor.offset)
        .with_selected(selected);
    frame.render_stateful_widget(list, area, &mut state);
    cursor.offset = state.offset();
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
    draw_list(frame, area, block, rows, &mut view.news);
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
    draw_list(frame, area, block, rows, &mut view.accounts);
}

/// The rows of `form`: name, login, password, then a check box for each privilege.
fn account_form_rows(form: &AccountForm) -> Vec<String> {
    let text = [
        format!("name:     {}", form.name),
        format!("login:    {}", form.login),
        format!("password: {}", form.password_text()),
    ];
    let checks = privileges().map(|(label, flag)| check_row(form.access.contains(flag), &label));
    text.into_iter().chain(checks).collect()
}

/// The rows of `form`: name, icon, automatic response, then a check box for each option.
fn options_form_rows(form: &OptionsForm) -> Vec<String> {
    let text = [
        format!("name:     {}", form.name),
        format!("icon:     {}", form.icon),
        format!("response: {}", form.automatic_response),
    ];
    let checks = OPTIONS
        .iter()
        .map(|(label, flag)| check_row(form.options.contains(*flag), label));
    text.into_iter().chain(checks).collect()
}

/// A form row with a check box, marked if `on`.
fn check_row(on: bool, label: &str) -> String {
    let mark = if on { 'x' } else { ' ' };
    format!("[{mark}] {label}")
}

/// The lines of `info`: size, type and creator if sent, and comment.
fn file_info_lines(info: &FileInfo) -> Vec<String> {
    let size = format!("size:    {} ({} bytes)", size_text(info.size), info.size);
    let kind = info.type_text.iter().map(|t| format!("type:    {t}"));
    let creator = info.creator_text.iter().map(|c| format!("creator: {c}"));
    let comment = format!("comment: {}", info.comment.replace('\r', "\n"));
    std::iter::once(size)
        .chain(kind)
        .chain(creator)
        .chain(std::iter::once(comment))
        .collect()
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
    let lines: Vec<UiLine> = hint_rows(view.hints(), area.width)
        .into_iter()
        .map(|row| {
            let spans = row.iter().flat_map(|(key, action)| {
                [
                    Span::styled(*key, Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(format!(" {action}  ")),
                ]
            });
            UiLine::from(spans.collect::<Vec<_>>())
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_modal(frame: &mut Frame, screen: Rect, modal: &Modal) {
    match modal {
        Modal::InstantMsg { name, input, .. } => {
            draw_input_modal(frame, screen, format!("message to {name}"), input);
        }
        Modal::Broadcast { input } => {
            draw_input_modal(frame, screen, "broadcast to every user".into(), input);
        }
        Modal::Upload { input } => {
            draw_input_modal(frame, screen, "upload a local file or folder".into(), input);
        }
        Modal::SaveAs { name, input } => {
            draw_input_modal(
                frame,
                screen,
                format!("{name} is downloaded, save as"),
                input,
            );
        }
        Modal::Confirm(conflict) => {
            draw_question(frame, screen, "resume?", conflict.question());
        }
        Modal::Disconnect { name, .. } => {
            draw_question(frame, screen, "disconnect?", format!("Disconnect {name}?"));
        }
        Modal::FileEdit { edit, name, input } => {
            draw_input_modal(frame, screen, edit.title(name), input);
        }
        Modal::DeleteFile { name } => {
            let text = format!("Delete {name} and everything in it?");
            draw_question(frame, screen, "delete?", text);
        }
        Modal::FileInfo(info) => {
            let text = file_info_lines(info).join("\n");
            draw_text_modal(frame, screen, 70, format!("info for {}", info.name), text);
        }
        Modal::Invite { from, .. } => {
            let text = format!("{from} invites you to a private chat.");
            draw_question(frame, screen, "private chat", text);
        }
        Modal::Subject { input, .. } => {
            draw_input_modal(frame, screen, "chat subject".into(), input);
        }
        Modal::PickUser {
            users, selected, ..
        } => {
            let area = centered(screen, 40, list_height(users.len()));
            let items = users.iter().map(|(_, name)| ListItem::new(&**name));
            draw_list_modal(frame, area, modal_block("invite to chat"), items, *selected);
        }
        Modal::PickDownload {
            downloads,
            selected,
        } => {
            let area = centered(screen, 50, list_height(downloads.len()));
            let items = downloads.iter().map(|(_, label)| ListItem::new(&**label));
            let block = modal_block("cancel a download");
            draw_list_modal(frame, area, block, items, *selected);
        }
        Modal::UserMenu {
            at,
            items,
            selected,
            ..
        } => {
            let area = menu_rect(*at, items, screen);
            let items = items.iter().map(|item| ListItem::new(item.label()));
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border(true));
            draw_list_modal(frame, area, block, items, *selected);
        }
        Modal::Article {
            title,
            poster,
            text,
            ..
        } => {
            let text = text.replace('\r', "\n");
            draw_text_modal(frame, screen, 76, format!("{title} ({poster})"), text);
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
            let area = centered(screen, 50, list_height(rows.len()));
            let title = match &form.old_login {
                Some(login) => format!("account {login}"),
                None => "new account".to_string(),
            };
            draw_list_modal(frame, area, modal_block(title), rows, form.row);
        }
        Modal::Options(form) => {
            let rows = options_form_rows(form);
            let area = centered(screen, 60, list_height(rows.len()));
            draw_list_modal(frame, area, modal_block("options"), rows, form.row);
        }
        Modal::OpenAccount { input } => {
            draw_input_modal(frame, screen, "open the account with login".into(), input);
        }
        Modal::DeleteAccount { login } => {
            let text = format!("Delete the account {login}?");
            draw_question(frame, screen, "delete?", text);
        }
        Modal::NewsName { category, input } => {
            let what = if *category { "category" } else { "bundle" };
            draw_input_modal(frame, screen, format!("new {what}"), input);
        }
        Modal::DeleteNews { name, .. } => {
            draw_question(frame, screen, "delete?", format!("Delete {name}?"));
        }
        Modal::Info { name, text } => {
            let text = text.replace('\r', "\n");
            draw_text_modal(frame, screen, 70, format!("info for {name}"), text);
        }
    }
}

/// The bordered, focused block of a modal.
fn modal_block<'a>(title: impl Into<UiLine<'a>>) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(border(true))
        .title(title)
}

/// The height of a modal list of `len` rows, with at least one row.
fn list_height(len: usize) -> u16 {
    u16::try_from(len.max(1))
        .unwrap_or(u16::MAX)
        .saturating_add(2)
}

/// Draw a modal that edits `input` under `title`.
fn draw_input_modal(frame: &mut Frame, screen: Rect, title: String, input: &str) {
    let area = centered(screen, 60, input_rows(input) + 2);
    frame.render_widget(Clear, area);
    draw_text_box(frame, area, modal_block(title), input, true);
}

/// Draw a modal that asks the one-line question `text`.
fn draw_question<'a>(frame: &mut Frame, screen: Rect, title: &'a str, text: impl Into<Text<'a>>) {
    let area = centered(screen, 60, 3);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(text).block(modal_block(title)), area);
}

/// Draw a modal that shows `text`, wrapped, under `title`.
fn draw_text_modal(frame: &mut Frame, screen: Rect, width: u16, title: String, text: String) {
    let height = list_height(text.lines().count()).min(screen.height.saturating_sub(2));
    let area = centered(screen, width, height);
    let paragraph = Paragraph::new(text)
        .block(modal_block(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

/// Draw a modal list of `items` in `block` with row `selected` highlighted.
fn draw_list_modal<'a>(
    frame: &mut Frame,
    area: Rect,
    block: Block<'a>,
    items: impl IntoIterator<Item = impl Into<ListItem<'a>>>,
    selected: usize,
) {
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw_to_text(modal: &Modal) -> String {
        let backend = ratatui::backend::TestBackend::new(70, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_modal(frame, frame.size(), modal))
            .unwrap();
        let buffer = terminal.backend().buffer();
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn draw_modal_shows_the_title_and_the_text() {
        let modal = Modal::DeleteNews {
            index: 0,
            name: "Old news".into(),
        };
        let text = draw_to_text(&modal);
        assert!(text.contains("delete?"));
        assert!(text.contains("Delete Old news?"));

        let text = draw_to_text(&Modal::Broadcast {
            input: "hello".into(),
        });
        assert!(text.contains("broadcast to every user"));
        assert!(text.contains("hello"));
    }

    #[test]
    fn hint_rows_keep_each_hint_whole() {
        let hints = [("Enter", "send"), ("Tab", "users"), ("Esc", "quit")];
        assert_eq!(hint_rows(&hints, 80), [&hints[..]]);
        assert_eq!(hint_rows(&hints, 23), [&hints[..2], &hints[2..]]);
        assert_eq!(hint_rows(&hints, 1).len(), 3);
    }

    #[test]
    fn size_text_picks_the_largest_unit_under_1024() {
        assert_eq!(size_text(512), "512 B");
        assert_eq!(size_text(1536), "1.5 KB");
        assert_eq!(size_text(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(size_text(u32::MAX), "4.0 GB");
    }

    #[test]
    fn file_info_lines_leave_out_what_the_server_does_not_send() {
        let info = FileInfo {
            name: "a.txt".into(),
            size: 2048,
            type_text: None,
            creator_text: Some("ttxt".into()),
            comment: "hi".into(),
        };
        assert_eq!(
            file_info_lines(&info),
            [
                "size:    2.0 KB (2048 bytes)",
                "creator: ttxt",
                "comment: hi"
            ]
        );
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
