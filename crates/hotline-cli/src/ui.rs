//! Rendering, and the screen areas that mouse clicks hit.

use hotline_client::FileInfo;
use hotline_proto::opcode::TransactionType;
use hotline_proto::text_encoding;
use hotline_proto::types::{FileNameWithInfo, UserFlags, UserNameWithInfo};
use hotline_proto::{Access, Field, FieldId, Opcode, Received, Transaction};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as UiLine, Span, Text};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Row, Table, Wrap};
use ratatui::Frame;

use crate::line::{Line, LineKind};
use crate::session::{ClientSession, Mode, NewsFolderNode, NewsThread};
use crate::view::{
    allows, contains, privileges, AccountForm, Focus, Hint, MenuItem, Modal, OptionsForm, Tab,
    View, OPTIONS,
};

/// Where each part of the main screen is drawn.
pub struct Areas {
    pub screen: Rect,
    pub tabs: Rect,
    /// The room row under the Chat tab.
    pub rooms: Rect,
    /// The chat lines or the news.
    pub body: Rect,
    pub input: Rect,
    pub users: Rect,
    pub hints: Rect,
}

/// The columns that `text` takes on screen.
fn text_width(text: &str) -> usize {
    Span::raw(text).width()
}

/// The most lines a text box shows before it scrolls.
const MAX_INPUT_ROWS: usize = 5;

/// The lines a text box needs to show `text`.
fn input_rows(text: &str) -> u16 {
    text.split('\n').count().min(MAX_INPUT_ROWS) as u16
}

/// `hints` laid out in rows no wider than `width`, keeping each hint whole.
fn hint_rows(hints: &[Hint], width: u16) -> Vec<&[Hint]> {
    let mut rows = Vec::new();
    let (mut start, mut used) = (0, 0);
    for (i, hint) in hints.iter().enumerate() {
        let len = text_width(hint.key) + text_width(hint.does) + 3;
        if used > 0 && used + len > usize::from(width) {
            rows.push(hints.get(start..i).unwrap_or_default());
            (start, used) = (i, 0);
        }
        used += len;
    }
    rows.push(hints.get(start..).unwrap_or_default());
    rows
}

/// `input` is the text in the input line, which grows to fit it. With `None`,
/// the input line has no rows. The hint line grows to fit `hints`. Without
/// `users`, the user list has no columns. Without `rooms`, the room row has
/// no rows.
pub fn areas(screen: Rect, input: Option<&str>, hints: &[Hint], users: bool, rooms: bool) -> Areas {
    let hint_rows = hint_rows(hints, screen.width).len() as u16;
    let [tabs, rooms, middle, hints] = screen.layout(&Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(rooms)),
        Constraint::Min(6),
        Constraint::Length(hint_rows),
    ]));
    let [left, users] = middle.layout(&Layout::horizontal([
        Constraint::Min(20),
        Constraint::Length(if users { 24 } else { 0 }),
    ]));
    let [body, input] = left.layout(&Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(input.map_or(0, |text| input_rows(text) + 2)),
    ]));
    Areas {
        screen,
        tabs,
        rooms,
        body,
        input,
        users,
        hints,
    }
}

/// The folder tree, the thread list, and the article preview of the
/// Threaded News tab in `body`.
pub fn news_areas(body: Rect) -> [Rect; 3] {
    let [folders, right] = body.layout(&Layout::horizontal([
        Constraint::Percentage(30),
        Constraint::Fill(1),
    ]));
    let [threads, preview] = right.layout(&Layout::vertical([
        Constraint::Percentage(50),
        Constraint::Fill(1),
    ]));
    [folders, threads, preview]
}

/// `rect` inside its border.
pub fn inner(rect: Rect) -> Rect {
    Block::bordered().inner(rect)
}

/// Each tab and its label: the fixed tabs, then one for each room. Each
/// of the first nine labels before the rooms starts with its number, the
/// key of the tab.
pub fn tab_labels(session: &ClientSession) -> Vec<(Tab, String)> {
    let fixed = Tab::FIXED
        .iter()
        .map(|&(tab, label)| (tab, label.to_string()));
    let admin = allows(session.granted(), Tab::Admin.enabled_by())
        .then(|| (Tab::Admin, Tab::ADMIN_LABEL.to_string()));
    let rooms = session
        .rooms
        .iter()
        .map(|room| (Tab::Room(room.id), room.label()));
    fixed
        .chain(admin)
        .zip(1..)
        .map(|((tab, label), n)| match n {
            1..=9 => (tab, format!("{n} {label}")),
            _ => (tab, label),
        })
        .chain(rooms)
        .collect()
}

/// Each tab in `labels` and the columns its label covers, drawn as ` Chat │ News `.
fn tab_columns(
    tabs: Rect,
    labels: &[(Tab, String)],
) -> impl Iterator<Item = (Tab, &str, std::ops::Range<u16>)> {
    labels.iter().scan(tabs.x, |x, (tab, label)| {
        let start = *x;
        let end = start + text_width(label) as u16 + 2;
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

/// The tab that gets a chat line or a news post from the push `tx`.
pub fn push_tab(session: &ClientSession, tx: &Transaction<Received>) -> Option<Tab> {
    match tx.r#type() {
        TransactionType::Push(Opcode::ChatMsg) => Some(
            tx.field(FieldId::ChatID)
                .map(Field::as_u32)
                .filter(|&id| session.room(id).is_some())
                .map_or(Tab::Chat, Tab::Room),
        ),
        TransactionType::Push(Opcode::NewMsg) => Some(Tab::News),
        _ => None,
    }
}

/// The revision of each tab that changes without the user: the chat, the
/// classic news, and each room.
pub fn tab_revisions(session: &ClientSession) -> impl Iterator<Item = (Tab, u64)> + '_ {
    let rooms = session
        .rooms
        .iter()
        .map(|room| (Tab::Room(room.id), room.lines.revision()));
    [
        (Tab::Chat, session.lines.revision()),
        (Tab::News, session.news_revision),
    ]
    .into_iter()
    .chain(rooms)
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

    let hints = view.hints(session.files.as_deref().unwrap_or_default());
    let room_row = view.room_row();
    let areas = areas(
        frame.area(),
        view.input(),
        &hints,
        view.shows_users(),
        !room_row.is_empty(),
    );
    draw_tabs(frame, areas.tabs, view, view.bar_tabs(), true);
    draw_tabs(frame, areas.rooms, view, &room_row, false);
    match view.tab {
        Tab::Chat => draw_messages(frame, areas.body, &session.lines, "Hotline"),
        Tab::News | Tab::Threaded if !view.may(view.tab.enabled_by()) => {
            let block = Block::bordered().title("news");
            let text = Paragraph::new("You are not allowed to read news.").block(block);
            frame.render_widget(text, areas.body);
        }
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
    if view.shows_users() {
        draw_users(frame, areas.users, session, view);
    }
    draw_hints(frame, areas.hints, view, &hints);
    if let Some(modal) = &view.modal {
        draw_modal(frame, areas.screen, modal, view.access);
    }
}

fn draw_agreement(frame: &mut Frame, text: &str) {
    let [text_area, hint_area] = frame.area().layout(&Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(3),
    ]));

    let paragraph = Paragraph::new(text_encoding::from_wire_lines(text))
        .block(Block::bordered().title("agreement"))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, text_area);

    let hint = Paragraph::new("press Enter to accept, Esc to quit").block(Block::bordered());
    frame.render_widget(hint, hint_area);
}

/// Draw `labels` as a row of tabs. In the tab `bar`, the Chat tab shows
/// as open and as changed for each room too.
fn draw_tabs(frame: &mut Frame, area: Rect, view: &View, labels: &[(Tab, String)], bar: bool) {
    let mut spans = Vec::new();
    for (tab, label, _) in tab_columns(area, labels) {
        if !spans.is_empty() {
            spans.push(Span::raw("│"));
        }
        let (shown, updated) = if bar {
            let under = |other: &Tab| other.top() == tab;
            (view.tab.top() == tab, view.updated.iter().any(under))
        } else {
            (tab == view.tab, view.updated.contains(&tab))
        };
        let style = match (shown, updated) {
            (true, _) => enabled_style(view.may(tab.enabled_by())).add_modifier(Modifier::REVERSED),
            _ if !view.may(tab.enabled_by()) => enabled_style(false),
            (false, true) => Style::default().fg(Color::Yellow),
            (false, false) => Style::default(),
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

    let paragraph = Paragraph::new(lines.get(start..).unwrap_or_default().to_vec())
        .block(Block::bordered().title(title))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn draw_news(frame: &mut Frame, area: Rect, session: &ClientSession, scroll: u16) {
    let text = match &session.news {
        Some(news) => text_encoding::from_wire_lines(news),
        None => "loading news".to_string(),
    };
    let paragraph = Paragraph::new(text)
        .block(Block::bordered().title("news"))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

fn draw_files(frame: &mut Frame, area: Rect, session: &ClientSession, view: &mut View) {
    let block = Block::bordered()
        .border_style(border(view.focus == Focus::Files))
        .title(format!("files {}", session.folder_text()));
    let Some(files) = &session.files else {
        frame.render_widget(Paragraph::new("loading files").block(block), area);
        return;
    };
    let items: Vec<ListItem> = files.iter().map(|f| ListItem::new(file_label(f))).collect();
    draw_list(frame, area, block, items, &mut view.files);
}

/// Draw `items` in `block` with the selection and scroll position of
/// `state`. The first row is selected when none is.
fn draw_list(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    items: Vec<ListItem>,
    state: &mut ListState,
) {
    if state.selected().is_none() {
        state.select_first();
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, area, state);
}

fn draw_threaded_news(frame: &mut Frame, area: Rect, session: &ClientSession, view: &mut View) {
    let [folders_area, threads_area, preview_area] = news_areas(area);
    draw_news_folders(frame, folders_area, session, view);
    draw_news_threads(frame, threads_area, session, view);
    draw_news_preview(frame, preview_area, session, view);
}

fn draw_news_folders(frame: &mut Frame, area: Rect, session: &ClientSession, view: &mut View) {
    let block = Block::bordered()
        .border_style(border(view.focus == Focus::NewsFolders))
        .title("folders");
    let Some(folders) = &session.news_folders else {
        frame.render_widget(Paragraph::new("loading news").block(block), area);
        return;
    };
    let rows = folders.iter().map(NewsFolderNode::cells).map(Row::new);
    let widths = [Constraint::Fill(1), Constraint::Length(5)];
    let table = Table::new(rows, widths)
        .block(block)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    if view.news_folders.selected().is_none() {
        view.news_folders.select_first();
    }
    frame.render_stateful_widget(table, area, &mut view.news_folders);
}

fn draw_news_threads(frame: &mut Frame, area: Rect, session: &ClientSession, view: &mut View) {
    let title = match &session.news_category {
        Some(path) => format!("threads /{}", path.components.join("/")),
        None => "threads".to_string(),
    };
    let block = Block::bordered()
        .border_style(border(view.focus == Focus::NewsThreads))
        .title(title);
    let cells: Vec<[String; 3]> = session.news_threads.iter().map(NewsThread::cells).collect();
    let date_width = cells
        .iter()
        .map(|[_, _, date]| text_width(date))
        .fold(text_width("date"), usize::max);
    let widths = [
        Constraint::Fill(1),
        Constraint::Length(16),
        Constraint::Length(date_width as u16),
    ];
    let header = Row::new(["title", "poster", "date"]);
    let table = Table::new(cells.into_iter().map(Row::new), widths)
        .header(header.style(Style::default().add_modifier(Modifier::BOLD)))
        .block(block)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    if view.news.selected().is_none() {
        view.news.select_first();
    }
    frame.render_stateful_widget(table, area, &mut view.news);
}

fn draw_news_preview(frame: &mut Frame, area: Rect, session: &ClientSession, view: &View) {
    let block = Block::bordered().border_style(border(view.focus == Focus::NewsPreview));
    let Some(article) = &session.news_preview else {
        frame.render_widget(block.title("preview"), area);
        return;
    };
    let text = text_encoding::from_wire_lines(&article.text);
    let paragraph = Paragraph::new(text)
        .block(block.title(article.heading()))
        .wrap(Wrap { trim: false })
        .scroll((view.preview_scroll, 0));
    frame.render_widget(paragraph, area);
}

fn draw_accounts(frame: &mut Frame, area: Rect, session: &ClientSession, view: &mut View) {
    let block = Block::bordered()
        .border_style(border(view.focus == Focus::Accounts))
        .title("accounts");
    let Some(accounts) = &session.accounts else {
        frame.render_widget(Paragraph::new("loading accounts").block(block), area);
        return;
    };
    let rows: Vec<ListItem> = accounts
        .iter()
        .map(|a| {
            ListItem::new(format!(
                "{} {}  ({})",
                account_circle(a.access),
                a.login,
                a.name
            ))
        })
        .collect();
    draw_list(frame, area, block, rows, &mut view.accounts);
}

/// The privileges that make an account an admin: all but `NO_AGREEMENT`.
const ALL_PRIV: Access = Access::all().difference(Access::NO_AGREEMENT);

/// The color of openline's account icon for `access`, as a circle: black
/// for an admin, red for a moderator, green for no privileges, and yellow
/// for a guest.
const fn account_circle(access: Access) -> &'static str {
    if access.contains(ALL_PRIV) {
        "⚫"
    } else if access.contains(Access::DISCON_USER) {
        "🔴"
    } else if access.is_empty() {
        "🟢"
    } else {
        "🟡"
    }
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
    let comment = format!("comment: {}", text_encoding::from_wire_lines(&info.comment));
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
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = f64::from(bytes) / 1024.0;
    let mut unit = "KB";
    for next in ["MB", "GB"] {
        if size < 1024.0 {
            break;
        }
        size /= 1024.0;
        unit = next;
    }
    format!("{size:.1} {unit}")
}

fn draw_input(frame: &mut Frame, area: Rect, view: &View) {
    let title = match view.tab {
        Tab::Chat | Tab::Room(_) => "message",
        Tab::News => "post",
        Tab::Files | Tab::Threaded | Tab::Admin => return,
    };
    let focused = view.focus == Focus::Input;
    let block = Block::bordered().border_style(border(focused)).title(title);
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
    let lines = u16::try_from(text.split('\n').count()).unwrap_or(u16::MAX);
    let rows = input_rows(text);
    let paragraph = Paragraph::new(text)
        .block(block)
        .scroll((lines.saturating_sub(rows), 0));
    frame.render_widget(paragraph, area);

    if cursor {
        let last = text.rsplit('\n').next().unwrap_or_default();
        let x = area.x + 1 + text_width(last) as u16;
        frame.set_cursor_position((x.min(area.right().saturating_sub(2)), area.y + rows));
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
            Block::bordered()
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

fn draw_hints(frame: &mut Frame, area: Rect, view: &View, hints: &[Hint]) {
    let lines: Vec<UiLine> = hint_rows(hints, area.width)
        .into_iter()
        .map(|row| {
            let spans = row.iter().flat_map(|hint| {
                let style = enabled_style(allows(view.access, hint.enabled_by));
                [
                    Span::styled(hint.key, style.add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" {}  ", hint.does), style),
                ]
            });
            UiLine::from(spans.collect::<Vec<_>>())
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// The plain style, or gray for a key or an item that is turned off.
fn enabled_style(enabled: bool) -> Style {
    if enabled {
        Style::default()
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn draw_modal(frame: &mut Frame, screen: Rect, modal: &Modal, access: Access) {
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
        Modal::Invite(invite) => {
            let text = format!("{} invites you to a private chat.", invite.from);
            draw_question(frame, screen, "private chat", text);
        }
        Modal::Subject { input, .. } => {
            draw_input_modal(frame, screen, "chat subject".into(), input);
        }
        Modal::PickUser {
            users, selected, ..
        } => {
            let area = screen.centered(
                Constraint::Length(40),
                Constraint::Length(list_height(users.len())),
            );
            let items = users.iter().map(|(_, name)| ListItem::new(&**name));
            draw_list_modal(frame, area, modal_block("invite to chat"), items, *selected);
        }
        Modal::PickDownload {
            downloads,
            selected,
        } => {
            let area = screen.centered(
                Constraint::Length(50),
                Constraint::Length(list_height(downloads.len())),
            );
            let items = downloads.iter().map(|(_, label)| ListItem::new(&**label));
            let block = modal_block("cancel a download");
            draw_list_modal(frame, area, block, items, *selected);
        }
        Modal::Palette(palette) => {
            let rows: Vec<ListItem> = palette
                .matches()
                .map(|(command, label)| {
                    let style = enabled_style(allows(access, command.enabled_by()));
                    ListItem::new(format!("{label:<38}{}", command.key())).style(style)
                })
                .collect();
            let area = screen.centered(
                Constraint::Length(46),
                Constraint::Length(list_height(rows.len())),
            );
            let block = match palette.input.as_str() {
                "" => modal_block(palette.title),
                input => modal_block(format!("> {input}")),
            };
            draw_list_modal(frame, area, block, rows, palette.selected);
        }
        Modal::UserMenu {
            at,
            items,
            selected,
            ..
        } => {
            let area = menu_rect(*at, items, screen);
            let items = items.iter().map(|item| {
                let style = enabled_style(allows(access, item.enabled_by()));
                ListItem::new(item.label()).style(style)
            });
            let block = Block::bordered().border_style(border(true));
            draw_list_modal(frame, area, block, items, *selected);
        }
        Modal::Article(article) => {
            let text = text_encoding::from_wire_lines(&article.text);
            draw_text_modal(frame, screen, 76, article.heading(), text);
        }
        Modal::Post {
            parent_id,
            title,
            body,
            on_body,
        } => {
            let area = screen.centered(
                Constraint::Length(70),
                Constraint::Length(input_rows(body) + 5),
            );
            let title_area = Rect::new(area.x, area.y, area.width, 3);
            let body_area = Rect::new(
                area.x,
                area.y + 3,
                area.width,
                area.height.saturating_sub(3),
            );
            let heading = if *parent_id == 0 { "new post" } else { "reply" };
            let title_block = Block::bordered()
                .border_style(border(!on_body))
                .title(heading);
            let body_block = Block::bordered()
                .border_style(border(*on_body))
                .title("text");
            frame.render_widget(Clear, area);
            draw_text_box(frame, title_area, title_block, title, !on_body);
            draw_text_box(frame, body_area, body_block, body, *on_body);
        }
        Modal::Account(form) => {
            let rows = account_form_rows(form);
            let area = screen.centered(
                Constraint::Length(50),
                Constraint::Length(list_height(rows.len())),
            );
            let title = match &form.old_login {
                Some(login) => format!("account {login}"),
                None => "new account".to_string(),
            };
            draw_list_modal(frame, area, modal_block(title), rows, form.row);
        }
        Modal::Options(form) => {
            let rows = options_form_rows(form);
            let area = screen.centered(
                Constraint::Length(60),
                Constraint::Length(list_height(rows.len())),
            );
            draw_list_modal(frame, area, modal_block("options"), rows, form.row);
        }
        Modal::OpenAccount { input } => {
            draw_input_modal(frame, screen, "open the account with login".into(), input);
        }
        Modal::DeleteAccount { login } => {
            let text = format!("Delete the account {login}?");
            draw_question(frame, screen, "delete?", text);
        }
        Modal::NewsName { kind, input } => {
            draw_input_modal(frame, screen, format!("new {}", kind.label()), input);
        }
        Modal::Quit => draw_question(frame, screen, "quit?", "Disconnect and quit hlc?"),
        Modal::DeleteNews { name, .. } => {
            draw_question(frame, screen, "delete?", format!("Delete {name}?"));
        }
        Modal::Info(info) => {
            let text = text_encoding::from_wire_lines(&info.text);
            draw_text_modal(frame, screen, 70, format!("info for {}", info.name), text);
        }
    }
}

/// The bordered, focused block of a modal.
fn modal_block<'a>(title: impl Into<UiLine<'a>>) -> Block<'a> {
    Block::bordered().border_style(border(true)).title(title)
}

/// The height of a modal list of `len` rows, with at least one row.
fn list_height(len: usize) -> u16 {
    u16::try_from(len.max(1))
        .unwrap_or(u16::MAX)
        .saturating_add(2)
}

/// Draw a modal that edits `input` under `title`.
fn draw_input_modal(frame: &mut Frame, screen: Rect, title: String, input: &str) {
    let area = screen.centered(
        Constraint::Length(60),
        Constraint::Length(input_rows(input) + 2),
    );
    frame.render_widget(Clear, area);
    draw_text_box(frame, area, modal_block(title), input, true);
}

/// Draw a modal that asks the one-line question `text`.
fn draw_question<'a>(frame: &mut Frame, screen: Rect, title: &'a str, text: impl Into<Text<'a>>) {
    let area = screen.centered(Constraint::Length(60), Constraint::Length(3));
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(text).block(modal_block(title)), area);
}

/// Draw a modal that shows `text`, wrapped, under `title`.
fn draw_text_modal(frame: &mut Frame, screen: Rect, width: u16, title: String, text: String) {
    let height = list_height(text.lines().count()).min(screen.height.saturating_sub(2));
    let area = screen.centered(Constraint::Length(width), Constraint::Length(height));
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
    use hotline_client::JoinedChat;
    use hotline_proto::text_encoding::EncodingHandle;

    use super::*;
    use crate::line::Scrollback;
    use crate::session::{ChatRoom, NewsTarget};

    #[test]
    fn account_circle_follows_openlines_icon_order() {
        assert_eq!(account_circle(ALL_PRIV), "⚫");
        assert_eq!(account_circle(Access::DISCON_USER), "🔴");
        assert_eq!(account_circle(ALL_PRIV - Access::ANY_NAME), "🔴");
        assert_eq!(account_circle(Access::empty()), "🟢");
        assert_eq!(account_circle(Access::READ_CHAT), "🟡");
    }

    fn draw_to_text(modal: &Modal) -> String {
        let backend = ratatui::backend::TestBackend::new(70, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_modal(frame, frame.area(), modal, Access::all()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        buffer.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn draw_text_box_puts_the_cursor_after_a_wide_character() {
        let backend = ratatui::backend::TestBackend::new(20, 3);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_text_box(frame, frame.area(), Block::bordered(), "\u{4f60}a", true);
            })
            .unwrap();
        terminal.backend_mut().assert_cursor_position((4, 1));
    }

    #[test]
    fn tab_labels_number_only_the_tab_bar() {
        let mut session = ClientSession::new("me".to_string(), 0, Default::default());
        session.rooms.push(ChatRoom {
            id: 7,
            subject: "".into(),
            users: Vec::new(),
            lines: Scrollback::default(),
        });
        let labels = tab_labels(&session);
        assert_eq!(labels.first(), Some(&(Tab::Chat, "1 Chat".to_string())));
        assert_eq!(labels.last(), Some(&(Tab::Room(7), "Chat 7".to_string())));
    }

    #[test]
    fn draw_tabs_marks_the_chat_tab_for_a_changed_room() {
        let mut view = View::default();
        (view.tab, view.access) = (Tab::News, Access::all());
        view.tabs.push((Tab::Room(7), "Chat 7".to_string()));
        view.updated.insert(Tab::Room(7));
        let backend = ratatui::backend::TestBackend::new(40, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_tabs(frame, frame.area(), &view, view.bar_tabs(), true))
            .unwrap();
        let chat = &terminal.backend().buffer().content[1];
        assert_eq!((chat.symbol(), chat.fg), ("C", Color::Yellow));
    }

    #[test]
    fn draw_agreement_shows_each_line() {
        let backend = ratatui::backend::TestBackend::new(20, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_agreement(frame, "one\rtwo"))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = |y| (1..4).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert_eq!((row(1), row(2)), ("one".into(), "two".into()));
    }

    #[test]
    fn draw_modal_shows_the_title_and_the_text() {
        let modal = Modal::DeleteNews {
            target: NewsTarget::Folder(0),
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
        let hints = [
            Hint::new("Enter", "send"),
            Hint::new("Tab", "users"),
            Hint::new("Esc", "quit"),
        ];
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

    #[test]
    fn push_tab_names_the_tab_of_a_chat_line_or_a_news_post() {
        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        session.rooms.push(
            JoinedChat {
                id: 7,
                subject: "".into(),
                users: Vec::new(),
            }
            .into(),
        );
        let push =
            |opcode, fields| Transaction::push(opcode, fields).received(text_encoding::DEFAULT);
        let chat = |chat_id| push(Opcode::ChatMsg, vec![Field::u32(FieldId::ChatID, chat_id)]);
        assert_eq!(push_tab(&session, &chat(7)), Some(Tab::Room(7)));
        assert_eq!(push_tab(&session, &chat(8)), Some(Tab::Chat));
        assert_eq!(
            push_tab(&session, &push(Opcode::ChatMsg, Vec::new())),
            Some(Tab::Chat)
        );
        assert_eq!(
            push_tab(&session, &push(Opcode::NewMsg, Vec::new())),
            Some(Tab::News)
        );
        assert_eq!(
            push_tab(&session, &push(Opcode::NotifyChatChangeUser, Vec::new())),
            None
        );
    }
}
