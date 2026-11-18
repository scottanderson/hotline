//! Rendering, and the screen areas that mouse clicks hit.

use hotline_client::FileInfo;
use hotline_proto::opcode::TransactionType;
use hotline_proto::text_encoding;
use hotline_proto::types::{FileNameWithInfo, UserFlags, UserNameWithInfo};
use hotline_proto::{Access, Field, FieldId, Opcode, Received, Transaction};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::merge::MergeStrategy;
use ratatui::text::{Line as UiLine, Span, Text};
use ratatui::widgets::{
    Block, Clear, Gauge, List, ListItem, ListState, Paragraph, Row, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Table, TableState, Wrap,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::line::{Line, LineKind};
use crate::session::{ClientSession, Mode, NewsFolderNode, NewsThread};
use crate::transfer::file_size_text;
use crate::view::{
    AccountForm, BookmarkRow, ConnectForm, Focus, Hint, MenuItem, Modal, OPTIONS, OptionsForm,
    QUEUE_TRANSFERS, Splits, Tab, TextBox, View, allows, contains, privileges,
};

/// Where each part of the main screen is drawn.
pub struct Areas {
    pub screen: Rect,
    pub tabs: Rect,
    /// The row under the Chat or the News tab.
    pub sub_row: Rect,
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

/// `n` as a screen coordinate, at most [`u16::MAX`].
fn to_u16(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// The columns of the label before the text of a form row.
const FORM_LABEL: usize = 10;

/// The most lines a text box shows before it scrolls.
const MAX_INPUT_ROWS: usize = 5;

/// The lines a text box needs to show `text`.
fn input_rows(text: &str) -> u16 {
    text.split('\n').count().min(MAX_INPUT_ROWS) as u16
}

/// `hints` laid out in rows no wider than `width`, keeping each hint whole.
fn hint_rows<'a>(hints: &'a [Hint<'a>], width: u16) -> Vec<&'a [Hint<'a>]> {
    let mut rows = Vec::new();
    let (mut start, mut used) = (0, 0_usize);
    for (i, hint) in hints.iter().enumerate() {
        let len = text_width(&hint.key)
            .saturating_add(text_width(hint.does))
            .saturating_add(3);
        if used > 0 && used.saturating_add(len) > usize::from(width) {
            rows.push(hints.get(start..i).unwrap_or_default());
            (start, used) = (i, 0);
        }
        used = used.saturating_add(len);
    }
    rows.push(hints.get(start..).unwrap_or_default());
    rows
}

/// The right column of the screen, which holds the user list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Side {
    /// The columns of the right column.
    pub width: u16,
}

#[cfg(test)]
impl Side {
    /// The default right column.
    pub const USERS: Self = Self { width: 24 };
}

/// `input` is the text in the input line, which grows to fit it. With `None`,
/// the input line has no rows. The hint line grows to fit `hints`. `side`
/// is the right column, and `None` hides it. Without `sub_row`, the row
/// under the tab bar has no rows.
pub fn areas(
    screen: Rect,
    input: Option<&str>,
    hints: &[Hint],
    side: Option<Side>,
    sub_row: bool,
) -> Areas {
    let hint_rows = hint_rows(hints, screen.width).len() as u16;
    let [tabs, sub_row, middle, hints] = screen.layout(&Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(sub_row)),
        Constraint::Min(6),
        Constraint::Length(hint_rows),
    ]));
    let [left, users] = middle.layout(
        &Layout::horizontal([
            Constraint::Min(20),
            Constraint::Length(side.map_or(0, |side| side.width)),
        ])
        .spacing(shared(side.is_some())),
    );
    let [body, input] = left.layout(
        &Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(input.map_or(0, |text| input_rows(text).saturating_add(2))),
        ])
        .spacing(shared(input.is_some())),
    );
    Areas {
        screen,
        tabs,
        sub_row,
        body,
        input,
        users,
        hints,
    }
}

/// The folder tree, the thread list, and the article preview of the
/// Threaded News tab in `body`, sized by `splits`.
pub fn news_areas(body: Rect, splits: Splits) -> [Rect; 3] {
    let folders = splits
        .news_folders
        .map_or(Constraint::Percentage(30), Constraint::Length);
    let threads = splits
        .news_threads
        .map_or(Constraint::Percentage(50), Constraint::Length);
    let [folders, right] =
        body.layout(&Layout::horizontal([folders, Constraint::Fill(1)]).spacing(shared(true)));
    let [threads, preview] =
        right.layout(&Layout::vertical([threads, Constraint::Fill(1)]).spacing(shared(true)));
    [folders, threads, preview]
}

/// The spacing between two panes: one shared border while the second
/// `shows`, and none when it does not.
const fn shared(shows: bool) -> i16 {
    if shows { -1 } else { 0 }
}

/// A bordered pane of the main screen, which joins the borders it shares
/// with the panes beside it.
const fn pane<'a>() -> Block<'a> {
    Block::bordered().merge_borders(MergeStrategy::Exact)
}

/// The area of the pane that has the focus, if it shows.
fn focus_area(view: &View, areas: &Areas) -> Option<Rect> {
    let news = view
        .shows_news_panes()
        .then(|| news_areas(areas.body, view.splits));
    match view.focus {
        Focus::Input => Some(areas.input),
        Focus::Users => Some(areas.users),
        Focus::Files | Focus::Accounts | Focus::Tasks => Some(areas.body),
        Focus::NewsFolders => news.map(|[folders, _, _]| folders),
        Focus::NewsThreads => news.map(|[_, threads, _]| threads),
        Focus::NewsPreview => news.map(|[_, _, preview]| preview),
    }
}

/// `rect` inside its border.
pub fn inner(rect: Rect) -> Rect {
    Block::bordered().inner(rect)
}

/// Each tab and its label: the tabs of the tab bar, then the threaded
/// news, then one for each room. Each of the first nine labels of the tab
/// bar starts with its number, the key of the tab.
pub fn tab_labels(session: &ClientSession) -> Vec<(Tab, String)> {
    let fixed = Tab::FIXED
        .iter()
        .map(|&(tab, label)| (tab, label.to_string()));
    let admin = allows(session.granted(), Tab::Admin.enabled_by())
        .then(|| (Tab::Admin, Tab::ADMIN_LABEL.to_string()));
    let threaded = (Tab::Threaded, Tab::THREADED_LABEL.to_string());
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
        .chain(std::iter::once(threaded))
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
        let end = start
            .saturating_add(text_width(label) as u16)
            .saturating_add(2);
        *x = end.saturating_add(1);
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
        .saturating_add(4);
    let height = (items.len() as u16).saturating_add(2);
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
    items.get(usize::from(y.saturating_sub(inner.y))).copied()
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
    let sub_row = view.sub_row();
    let areas = areas(
        frame.area(),
        view.input().map(TextBox::as_str),
        &hints,
        view.side(),
        !sub_row.is_empty(),
    );
    draw_hints(frame, areas.hints, view, &hints);
    draw_tabs(frame, areas.tabs, view, view.bar_tabs(), true);
    draw_tabs(frame, areas.sub_row, view, &sub_row, false);
    match view.tab {
        Tab::Chat => draw_messages(frame, areas.body, &session.lines, "Hotline"),
        Tab::News | Tab::Threaded if !view.may(view.tab.enabled_by()) => {
            let block = pane().title("news");
            let text = Paragraph::new("You are not allowed to read news.").block(block);
            frame.render_widget(text, areas.body);
        }
        Tab::News => draw_news(frame, areas.body, session, view.news_scroll),
        Tab::Files => draw_files(frame, areas.body, session, view),
        Tab::Threaded => draw_threaded_news(frame, areas.body, session, view),
        Tab::Tasks => draw_tasks(frame, areas.body, view),
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
    // The focused pane's border goes on top of the borders it shares.
    if let Some(area) = focus_area(view, &areas) {
        frame.render_widget(pane().border_style(border(true)), area);
    }
    if view.tab == Tab::Tasks {
        draw_task_scrollbar(frame, areas.body, view);
    }
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
    let block = pane().title(title);
    let inner = block.inner(area);
    // Scroll to the bottom: only the newest lines that fill the pane need a style.
    let mut tail = Vec::new();
    let mut rows = 0;
    for line in lines.iter().rev() {
        if rows >= usize::from(inner.height) {
            break;
        }
        let line = styled_line(line);
        let paragraph = Paragraph::new(line.clone()).wrap(Wrap { trim: false });
        rows = rows.saturating_add(paragraph.line_count(inner.width).max(1));
        tail.push(line);
    }
    tail.reverse();
    let scroll = rows.saturating_sub(usize::from(inner.height));
    let paragraph = Paragraph::new(tail)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0));
    frame.render_widget(paragraph, area);
}

/// `line` in the style of its kind.
fn styled_line(line: &Line) -> UiLine<'_> {
    let style = match line.kind {
        LineKind::Chat => Style::default(),
        LineKind::System => Style::default().fg(Color::DarkGray),
        LineKind::Error => Style::default().fg(Color::Red),
    };
    UiLine::from(Span::styled(&*line.text, style))
}

fn draw_news(frame: &mut Frame, area: Rect, session: &ClientSession, scroll: u16) {
    let text = match &session.news {
        Some(news) => text_encoding::from_wire_lines(news),
        None => "loading news".to_string(),
    };
    let paragraph = Paragraph::new(text)
        .block(pane().title("news"))
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

fn draw_files(frame: &mut Frame, area: Rect, session: &ClientSession, view: &mut View) {
    let block = pane()
        .border_style(border(view.focus == Focus::Files))
        .title(format!("files {}", session.folder_text()));
    let Some(files) = &session.files else {
        frame.render_widget(Paragraph::new("loading files").block(block), area);
        return;
    };
    let cells: Vec<[String; 2]> = files.iter().map(file_cells).collect();
    let size_width = cells
        .iter()
        .map(|[_, size]| text_width(size))
        .fold(0, usize::max);
    let rows = files.iter().zip(cells).map(|(file, cells)| {
        let row = Row::new(cells);
        if &file.file_type == b"alis" {
            row.style(Style::default().fg(Color::DarkGray))
        } else {
            row
        }
    });
    let widths = [Constraint::Fill(1), Constraint::Length(size_width as u16)];
    let table = Table::new(rows, widths)
        .block(block)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    if view.files.selected().is_none() {
        view.files.select_first();
    }
    frame.render_stateful_widget(table, area, &mut view.files);
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
    let [folders_area, threads_area, preview_area] = news_areas(area, view.splits);
    draw_news_folders(frame, folders_area, session, view);
    draw_news_threads(frame, threads_area, session, view);
    draw_news_preview(frame, preview_area, session, view);
}

fn draw_news_folders(frame: &mut Frame, area: Rect, session: &ClientSession, view: &mut View) {
    let block = pane()
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
    let block = pane()
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
    let block = pane().border_style(border(view.focus == Focus::NewsPreview));
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
    let block = pane()
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
        text_row("name:", form.name.as_str()),
        text_row("login:", form.login.as_str()),
        text_row("password:", &form.password_text()),
    ];
    let checks = privileges().map(|(label, flag)| check_row(form.access.contains(flag), &label));
    text.into_iter().chain(checks).collect()
}

/// The rows of `form`: name, icon, automatic response, then a check box for each option.
fn options_form_rows(form: &OptionsForm) -> Vec<String> {
    let text = [
        text_row("name:", form.name.as_str()),
        text_row("icon:", form.icon.as_str()),
        text_row("response:", form.automatic_response.as_str()),
    ];
    let checks = OPTIONS
        .iter()
        .map(|(label, flag)| check_row(form.options.contains(*flag), label));
    let queue = check_row(form.queue_transfers, QUEUE_TRANSFERS);
    text.into_iter()
        .chain(checks)
        .chain(std::iter::once(queue))
        .collect()
}

/// The rows of `form`: the server, the login, and the password.
fn connect_form_rows(form: &ConnectForm) -> Vec<UiLine<'static>> {
    vec![
        hint_row("server:", form.address.as_str(), "host:port"),
        hint_row("login:", form.login.as_str(), "guest"),
        text_row("password:", &form.password_text()).into(),
    ]
}

/// A form row with `label`, then `text`, or `hint` in gray when `text` is
/// empty.
fn hint_row(label: &str, text: &str, hint: &'static str) -> UiLine<'static> {
    match text {
        "" => UiLine::from(vec![
            Span::raw(format!("{label:FORM_LABEL$}")),
            Span::styled(hint, HINT),
        ]),
        text => text_row(label, text).into(),
    }
}

/// The style of a hint in an empty text box.
const HINT: Style = Style::new().fg(Color::DarkGray);

/// The column of the cursor in the text of the selected row of `form`.
fn connect_form_cursor(form: &ConnectForm) -> usize {
    match form.row {
        0 => text_width(form.address.before_cursor()),
        1 => text_width(form.login.before_cursor()),
        _ => form.password.before_cursor().graphemes(true).count(),
    }
}

/// A form row with `label`, then `text`.
fn text_row(label: &str, text: &str) -> String {
    format!("{label:FORM_LABEL$}{text}")
}

/// The column of the cursor in the text of the selected row of `form`,
/// `None` on a privilege row.
fn account_form_cursor(form: &AccountForm) -> Option<usize> {
    match form.row {
        0 => Some(text_width(form.name.before_cursor())),
        1 => Some(text_width(form.login.before_cursor())),
        2 => Some(form.password.as_ref().map_or(0, |password| {
            password.before_cursor().graphemes(true).count()
        })),
        _ => None,
    }
}

/// The column of the cursor in the text of the selected row of `form`,
/// `None` on an option row.
fn options_form_cursor(form: &OptionsForm) -> Option<usize> {
    let text = match form.row {
        0 => &form.name,
        1 => &form.icon,
        2 => &form.automatic_response,
        _ => return None,
    };
    Some(text_width(text.before_cursor()))
}

/// Put the cursor `column` columns into the text of row `row` of the form
/// drawn in `area`.
fn set_form_cursor(frame: &mut Frame, area: Rect, row: usize, column: usize) {
    let inner = inner(area);
    let x = inner
        .x
        .saturating_add(to_u16(FORM_LABEL.saturating_add(column)));
    let y = inner.y.saturating_add(to_u16(row));
    frame.set_cursor_position((x.min(inner.right().saturating_sub(1)), y));
}

/// A form row with a check box, marked if `on`.
fn check_row(on: bool, label: &str) -> String {
    let mark = if on { 'x' } else { ' ' };
    format!("[{mark}] {label}")
}

/// The lines of `info`: size, type and creator if sent, and comment.
fn file_info_lines(info: &FileInfo) -> Vec<String> {
    let size = format!(
        "size:    {} ({} bytes)",
        file_size_text(info.size.into()),
        info.size
    );
    let kind = info.type_text.iter().map(|t| format!("type:    {t}"));
    let creator = info.creator_text.iter().map(|c| format!("creator: {c}"));
    let comment = format!("comment: {}", text_encoding::from_wire_lines(&info.comment));
    std::iter::once(size)
        .chain(kind)
        .chain(creator)
        .chain(std::iter::once(comment))
        .collect()
}

/// The cells of a file row: the icon and the name, and the size. A
/// folder shows its item count, and an alias shows `-`.
fn file_cells(file: &FileNameWithInfo) -> [String; 2] {
    let size = match &file.file_type {
        b"fldr" => format!("({})", file.size),
        b"alis" => "-".to_string(),
        _ => file_size_text(file.size.into()),
    };
    [
        format!("{} {}", file_icon(&file.file_type), file.name),
        size,
    ]
}

/// The emoji of a file with the Mac type `file_type`.
const fn file_icon(file_type: &[u8; 4]) -> &'static str {
    match file_type {
        b"fldr" => "📁",
        b"HTft" => "⏳",
        b"SITD" | b"SIT!" | b"SIT5" | b"ZIP " => "📦",
        b"TEXT" => "📝",
        b"ttro" => "📃",
        b"PICT" | b"JPEG" | b"GIFf" | b"8BPS" | b"BMP " => "📷",
        b"DEXE" | b"APPL" => "🚀",
        b"HTbm" | b"HTtb" | b"HTss" => "🔖",
        b"Seg1" | b"Seg2" | b"Seg3" | b"SegN" => "🧩",
        b"alis" => "🔗",
        b"dImg" | b"rohd" => "💿",
        b"Mp3 " | b"MP3 " | b"mp3!" | b"SwaT" | b"RAE " | b"AIFF" | b"WAVE" | b"MiDi" => "🎵",
        b"MooV" | b"MPEG" | b"AVI " | b"VfW " => "🎬",
        b"SWFL" => "⚡",
        _ => "📄",
    }
}

fn draw_input(frame: &mut Frame, area: Rect, view: &View) {
    let title = match view.tab {
        Tab::Chat | Tab::Room(_) => "message",
        Tab::News => "post",
        Tab::Files | Tab::Threaded | Tab::Tasks | Tab::Admin => return,
    };
    let focused = view.focus == Focus::Input;
    let block = pane().border_style(border(focused)).title(title);
    draw_text_box(
        frame,
        area,
        block,
        view.input().unwrap_or(&TextBox::new()),
        focused && view.modal.is_none(),
    );
}

/// Draw the modal box `title` with `input`, or with `hint` in gray when
/// `input` is empty.
fn draw_hint_input_modal(
    frame: &mut Frame,
    screen: Rect,
    title: &str,
    input: &TextBox,
    hint: &str,
) {
    let area = screen.centered(Constraint::Length(60), Constraint::Length(3));
    frame.render_widget(Clear, area);
    let block = modal_block(title);
    if input.is_empty() {
        let inner = block.inner(area);
        frame.render_widget(Paragraph::new(Span::styled(hint, HINT)).block(block), area);
        frame.set_cursor_position(inner.as_position());
    } else {
        draw_text_box(frame, area, block, input, true);
    }
}

/// Draw `text` scrolled to show its cursor, and put the terminal cursor
/// there if `cursor`.
fn draw_text_box(frame: &mut Frame, area: Rect, block: Block, text: &TextBox, cursor: bool) {
    let inner = block.inner(area);
    let before = text.before_cursor();
    let row = before.matches('\n').count();
    let column = text_width(before.rsplit('\n').next().unwrap_or_default());
    let scroll_y = row
        .saturating_add(1)
        .saturating_sub(usize::from(inner.height));
    let scroll_x = column
        .saturating_add(1)
        .saturating_sub(usize::from(inner.width));
    let paragraph = Paragraph::new(text.as_str())
        .block(block)
        .scroll((to_u16(scroll_y), to_u16(scroll_x)));
    frame.render_widget(paragraph, area);

    if cursor {
        let x = to_u16(column.saturating_sub(scroll_x));
        let y = to_u16(row.saturating_sub(scroll_y));
        frame.set_cursor_position((inner.x.saturating_add(x), inner.y.saturating_add(y)));
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
            if u.name == session.name {
                style = style.add_modifier(Modifier::BOLD);
            }
            ListItem::new(Span::styled(label, style))
        })
        .collect();

    let list = List::new(items)
        .block(
            pane()
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

/// Draw the task list. The title counts the tasks, and the transfers that
/// wait in `hlc`'s queue.
fn draw_tasks(frame: &mut Frame, area: Rect, view: &mut View) {
    let items: Vec<ListItem> = view
        .tasks
        .iter()
        .map(|row| {
            let mut lines = vec![UiLine::from(&*row.label)];
            lines.extend(row.ratio.map(|_| UiLine::default()));
            ListItem::new(lines)
        })
        .collect();
    let focused = view.focus == Focus::Tasks;
    let queued = view.tasks.iter().filter(|row| row.queued).count();
    let title = match queued {
        0 => format!("tasks ({})", view.tasks.len()),
        queued => format!("tasks ({}, {queued} queued)", view.tasks.len()),
    };
    let list = List::new(items)
        .block(pane().border_style(border(focused)).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let selected = view.selected_task().map(|_| view.task_selected());
    let mut state = ListState::default()
        .with_offset(view.tasks_offset)
        .with_selected(selected.filter(|_| focused));
    frame.render_stateful_widget(list, area, &mut state);
    view.tasks_offset = state.offset();
    let inner = inner(area);
    let mut y = inner.y;
    for row in view.tasks.iter().skip(view.tasks_offset) {
        if y >= inner.bottom() {
            break;
        }
        let gauge_y = y.saturating_add(1);
        if let Some(ratio) = row.ratio.filter(|_| gauge_y < inner.bottom()) {
            let gauge = Rect {
                y: gauge_y,
                height: 1,
                ..inner
            };
            frame.render_widget(task_gauge(ratio), gauge);
        }
        y = y.saturating_add(row.height());
    }
}

/// Draw a scroll bar on the right border of the task list while its rows
/// do not all fit.
fn draw_task_scrollbar(frame: &mut Frame, area: Rect, view: &View) {
    let shown = usize::from(inner(area).height);
    let lines = view.task_lines();
    if lines <= shown {
        return;
    }
    let above = view
        .tasks
        .iter()
        .take(view.tasks_offset)
        .map(|row| usize::from(row.height()))
        .sum();
    let mut state = ScrollbarState::new(lines.saturating_sub(shown).saturating_add(1))
        .viewport_content_length(shown)
        .position(above);
    let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None);
    let track = area.inner(Margin {
        vertical: 1,
        horizontal: 0,
    });
    frame.render_stateful_widget(bar, track, &mut state);
}

/// The progress bar of a task that is `ratio` done, in eighths of a cell.
fn task_gauge(ratio: f64) -> Gauge<'static> {
    Gauge::default()
        .ratio(ratio)
        .use_unicode(true)
        .label("")
        .gauge_style(Style::default().fg(Color::Cyan).bg(Color::DarkGray))
}

fn draw_hints(frame: &mut Frame, area: Rect, view: &View, hints: &[Hint]) {
    let lines: Vec<UiLine> = hint_rows(hints, area.width)
        .into_iter()
        .map(|row| {
            let spans = row.iter().flat_map(|hint| {
                let style = enabled_style(allows(view.access, hint.enabled_by));
                [
                    Span::styled(&*hint.key, style.add_modifier(Modifier::BOLD)),
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
        Modal::Bookmarks { rows, selected } => draw_bookmarks(frame, screen, rows, *selected),
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
        Modal::Palette(palette) => {
            let rows: Vec<ListItem> = palette
                .matches()
                .map(|entry| {
                    let style = enabled_style(allows(access, entry.command.enabled_by()));
                    ListItem::new(format!("{:<38}{}", entry.label, entry.key)).style(style)
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
            if !palette.input.is_empty() {
                let column = text_width(palette.input.before_cursor());
                let x = area.x.saturating_add(to_u16(column.saturating_add(3)));
                frame.set_cursor_position((x.min(area.right().saturating_sub(2)), area.y));
            }
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
                Constraint::Length(input_rows(body.as_str()).saturating_add(5)),
            );
            let title_area = Rect::new(area.x, area.y, area.width, 3);
            let body_area = Rect::new(
                area.x,
                area.y.saturating_add(3),
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
            if let Some(column) = account_form_cursor(form) {
                set_form_cursor(frame, area, form.row, column);
            }
        }
        Modal::Options(form) => {
            let rows = options_form_rows(form);
            let area = screen.centered(
                Constraint::Length(60),
                Constraint::Length(list_height(rows.len())),
            );
            draw_list_modal(frame, area, modal_block("options"), rows, form.row);
            if let Some(column) = options_form_cursor(form) {
                set_form_cursor(frame, area, form.row, column);
            }
        }
        Modal::Connect(form) => {
            let rows = connect_form_rows(form);
            let area = screen.centered(
                Constraint::Length(60),
                Constraint::Length(list_height(rows.len())),
            );
            draw_list_modal(frame, area, modal_block("connect to"), rows, form.row);
            set_form_cursor(frame, area, form.row, connect_form_cursor(form));
        }
        Modal::BookmarkName { form, input } => {
            let hint = form.default_name().unwrap_or_default();
            draw_hint_input_modal(frame, screen, "save bookmark as", input, &hint);
        }
        Modal::OpenAccount { input } => {
            draw_input_modal(frame, screen, "open the account with login".into(), input);
        }
        Modal::DeleteBookmark { rows, selected } => {
            let name = rows.get(*selected).map_or("", |row| &*row.name);
            draw_question(
                frame,
                screen,
                "delete?",
                format!("Delete the bookmark {name}?"),
            );
        }
        Modal::DeleteAccount { login } => {
            let text = format!("Delete the account {login}?");
            draw_question(frame, screen, "delete?", text);
        }
        Modal::NewsName { kind, input } => {
            draw_input_modal(frame, screen, format!("new {}", kind.label()), input);
        }
        Modal::Quit => draw_question(frame, screen, "quit?", "Disconnect and quit hlc?"),
        Modal::Disconnected(reason) => {
            draw_text_modal(frame, screen, 60, reason.title().into(), reason.text());
        }
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

/// Draw the bookmarks as a table. A star marks the bookmark that connects
/// at the start.
fn draw_bookmarks(frame: &mut Frame, screen: Rect, rows: &[BookmarkRow], selected: usize) {
    let area = screen.centered(
        Constraint::Length(72),
        Constraint::Length(list_height(rows.len()).saturating_add(1)),
    );
    frame.render_widget(Clear, area);
    let block = modal_block("bookmarks");
    if rows.is_empty() {
        let text = "No bookmarks. The Connect to form saves one.";
        frame.render_widget(Paragraph::new(text).style(HINT).block(block), area);
        return;
    }
    let header = Row::new(["", "Name", "Address", "Login"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    let rows = rows.iter().map(|row| {
        let mark = if row.auto_connect { "*" } else { "" };
        Row::new([mark, &*row.name, &*row.address, &*row.login])
    });
    let widths = [
        Constraint::Length(1),
        Constraint::Fill(2),
        Constraint::Fill(2),
        Constraint::Fill(1),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected(Some(selected));
    frame.render_stateful_widget(table, area, &mut state);
}

/// Draw a modal that edits `input` under `title`.
fn draw_input_modal(frame: &mut Frame, screen: Rect, title: String, input: &TextBox) {
    let area = screen.centered(
        Constraint::Length(60),
        Constraint::Length(input_rows(input.as_str()).saturating_add(2)),
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
    let paragraph = Paragraph::new(text)
        .block(modal_block(title))
        .wrap(Wrap { trim: false });
    let width = width.min(screen.width);
    // line_count adds the border rows, but wraps at the full width it gets.
    let rows = paragraph.line_count(width.saturating_sub(2)).max(3);
    let height = u16::try_from(rows)
        .unwrap_or(u16::MAX)
        .min(screen.height.saturating_sub(2));
    let area = screen.centered(Constraint::Length(width), Constraint::Length(height));
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
    use crate::config::{Bookmark, Config};
    use crate::line::Scrollback;
    use crate::session::{ChatRoom, Disconnect, NewsTarget};
    use crate::tasks::{Stop, TaskId, TaskRow};

    #[test]
    fn areas_share_each_border_between_panes() {
        let areas = areas(
            Rect::new(0, 0, 80, 24),
            Some(""),
            &[],
            Some(Side::USERS),
            false,
        );
        assert_eq!(areas.users.x, areas.body.right() - 1);
        assert_eq!(areas.input.y, areas.body.bottom() - 1);
        assert_eq!(areas.input.bottom(), areas.hints.y);

        let [folders, threads, preview] = news_areas(areas.body, Splits::default());
        assert_eq!(threads.x, folders.right() - 1);
        assert_eq!(preview.y, threads.bottom() - 1);
    }

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

    /// The right border of the task list after a draw of `count` tasks.
    fn task_box_right_border(count: u64) -> String {
        let mut view = View::default();
        let rows = (1..=count).map(|id| TaskRow {
            id: TaskId::Request(id),
            label: format!("task {id}"),
            stop: None,
            queued: false,
            ratio: None,
        });
        view.set_tasks(rows.collect());
        let backend = ratatui::backend::TestBackend::new(20, 7);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_tasks(frame, frame.area(), &mut view);
                draw_task_scrollbar(frame, frame.area(), &view);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (1..6).map(|y| buffer[(19, y)].symbol()).collect()
    }

    #[test]
    fn draw_task_scrollbar_shows_only_while_the_rows_do_not_fit() {
        assert_eq!(task_box_right_border(5), "│││││");
        assert_ne!(task_box_right_border(8), "│││││");
        assert!(task_box_right_border(8).starts_with('█'));
    }

    #[test]
    fn draw_tasks_puts_a_gauge_under_a_task_of_known_size() {
        let mut view = View::default();
        view.set_tasks(vec![
            TaskRow {
                id: TaskId::Transfer(1),
                label: "up".into(),
                stop: Some(Stop::Cancel),
                queued: false,
                ratio: Some(0.3),
            },
            TaskRow {
                id: TaskId::Request(2),
                label: "list".into(),
                stop: Some(Stop::Cancel),
                queued: false,
                ratio: None,
            },
        ]);
        let backend = ratatui::backend::TestBackend::new(14, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_tasks(frame, frame.area(), &mut view))
            .unwrap();
        let row = |y: u16| -> String {
            (1..13)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol().to_string())
                .collect()
        };
        assert_eq!(row(1).trim_end(), "up");
        assert_eq!(row(2).trim_end(), "███▋");
        assert_eq!(row(3).trim_end(), "list");
    }

    #[test]
    fn draw_text_box_puts_the_cursor_after_a_wide_character() {
        let backend = ratatui::backend::TestBackend::new(20, 3);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let text = TextBox::from("\u{4f60}a");
                draw_text_box(frame, frame.area(), Block::bordered(), &text, true);
            })
            .unwrap();
        terminal.backend_mut().assert_cursor_position((4, 1));
    }

    #[test]
    fn draw_text_box_puts_the_cursor_inside_the_text() {
        let backend = ratatui::backend::TestBackend::new(20, 4);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut text = TextBox::from("ab\ncd");
        for _ in 0..4 {
            text.edit(ratatui::crossterm::event::KeyCode::Left.into());
        }
        terminal
            .draw(|frame| draw_text_box(frame, frame.area(), Block::bordered(), &text, true))
            .unwrap();
        terminal.backend_mut().assert_cursor_position((2, 1));
    }

    #[test]
    fn draw_text_box_scrolls_a_long_line_to_the_cursor() {
        let backend = ratatui::backend::TestBackend::new(6, 3);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let text = TextBox::from("abcdefgh");
        terminal
            .draw(|frame| draw_text_box(frame, frame.area(), Block::bordered(), &text, true))
            .unwrap();
        terminal.backend_mut().assert_cursor_position((4, 1));
        let row: String = (1..5)
            .map(|x| terminal.backend().buffer()[(x, 1)].symbol().to_string())
            .collect();
        assert_eq!(row, "fgh ");
    }

    #[test]
    fn draw_text_box_scrolls_back_to_a_cursor_at_the_start() {
        let backend = ratatui::backend::TestBackend::new(6, 3);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut text = TextBox::from("abcdefgh");
        assert!(text.edit(ratatui::crossterm::event::KeyCode::Home.into()));
        terminal
            .draw(|frame| draw_text_box(frame, frame.area(), Block::bordered(), &text, true))
            .unwrap();
        terminal.backend_mut().assert_cursor_position((1, 1));
        let row: String = (1..5)
            .map(|x| terminal.backend().buffer()[(x, 1)].symbol().to_string())
            .collect();
        assert_eq!(row, "abcd");
    }

    /// The text and the gray text of `modal`, drawn on an 80 by 24 screen.
    fn draw_gray(modal: &Modal) -> (String, String) {
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_modal(frame, frame.area(), modal, Access::all()))
            .unwrap();
        let cells = &terminal.backend().buffer().content;
        let text = cells.iter().map(|cell| cell.symbol()).collect();
        let gray = cells
            .iter()
            .filter(|cell| cell.fg == Color::DarkGray)
            .map(|cell| cell.symbol())
            .collect();
        (text, gray)
    }

    #[test]
    fn connect_form_rows_show_a_gray_hint_in_an_empty_row() {
        let (_, gray) = draw_gray(&Modal::Connect(ConnectForm::new("", "", "")));
        assert_eq!(gray, "host:portguest");
        let (text, gray) = draw_gray(&Modal::Connect(ConnectForm::new("a", "bob", "")));
        assert_eq!(gray, "");
        assert!(text.contains("bob"), "{text}");
    }

    #[test]
    fn bookmark_name_shows_the_default_name_in_gray_when_empty() {
        let form = ConnectForm::new("example.com:6000", "bob", "");
        let modal = Modal::BookmarkName {
            form: form.clone(),
            input: "".into(),
        };
        assert_eq!(draw_gray(&modal).1, "bob@example.com:6000");
        let modal = Modal::BookmarkName {
            form,
            input: "Home".into(),
        };
        let (text, gray) = draw_gray(&modal);
        assert_eq!(gray, "");
        assert!(text.contains("Home"), "{text}");
    }

    #[test]
    fn draw_modal_puts_the_cursor_in_the_selected_form_row() {
        let mut form = AccountForm::blank();
        form.row = 1;
        form.login = "bob".into();
        let backend = ratatui::backend::TestBackend::new(70, 60);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let modal = Modal::Account(form);
        let mut area = Rect::default();
        terminal
            .draw(|frame| {
                draw_modal(frame, frame.area(), &modal, Access::all());
                let rows = list_height(AccountForm::rows());
                area = frame
                    .area()
                    .centered(Constraint::Length(50), Constraint::Length(rows));
            })
            .unwrap();
        let x = area.x + 1 + to_u16(FORM_LABEL) + 3;
        terminal
            .backend_mut()
            .assert_cursor_position((x, area.y + 2));
    }

    #[test]
    fn tab_labels_number_only_the_tab_bar() {
        let mut session = ClientSession::new("me", 0, Default::default());
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
    fn tab_labels_hold_the_threaded_news_after_the_tab_bar() {
        let session = ClientSession::new("me", 0, Default::default());
        let labels = tab_labels(&session);
        assert_eq!(
            labels.last(),
            Some(&(Tab::Threaded, Tab::THREADED_LABEL.to_string()))
        );
    }

    #[test]
    fn draw_messages_shows_the_newest_line_under_a_wrapped_one() {
        let lines = [
            Line::new("old", LineKind::Chat),
            Line::new("x".repeat(25), LineKind::Chat),
            Line::new("new", LineKind::Chat),
        ];
        let backend = ratatui::backend::TestBackend::new(12, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_messages(frame, frame.area(), &lines, ""))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = |y| (1..11).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert_eq!(
            [row(1), row(2), row(3)],
            ["xxxxxxxxxx", "xxxxx     ", "new       "]
        );
    }

    #[test]
    fn draw_text_modal_grows_to_fit_each_wrapped_row() {
        let backend = ratatui::backend::TestBackend::new(12, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let text = "aaaa bbbb cccc".to_string();
        terminal
            .draw(|frame| draw_text_modal(frame, frame.area(), 6, String::new(), text))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let column = (0..8).map(|y| buffer[(4, y)].symbol()).collect::<String>();
        assert_eq!(column, "  ─abc─ ");
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

        let config = Config {
            bookmarks: vec![Bookmark {
                name: None,
                address: "example.com".into(),
                login: "alice".into(),
                password: "".into(),
            }],
            auto_connect: Some("alice@example.com".into()),
            ..Config::default()
        };
        let text = draw_to_text(&Modal::bookmarks(&config, 0));
        assert!(text.contains("* alice@example.com"));
        let text = draw_to_text(&Modal::bookmarks(&Config::default(), 0));
        assert!(text.contains("No bookmarks."));
        let Modal::Bookmarks { rows, selected } = Modal::bookmarks(&config, 0) else {
            panic!("no bookmarks");
        };
        let text = draw_to_text(&Modal::DeleteBookmark { rows, selected });
        assert!(text.contains("Delete the bookmark alice@example.com?"));

        let reason = Disconnect::Message("back soon".into());
        let text = draw_to_text(&Modal::Disconnected(reason));
        assert!(text.contains("Disconnected"));
        assert!(text.contains("You have been disconnected:"));
        assert!(text.contains("back soon"));

        let text = draw_to_text(&Modal::Disconnected(Disconnect::Closed));
        assert!(text.contains("Connection Closed"));
        assert!(text.contains("The connection with the server has been closed."));
        assert!(text.contains("connecting later."));

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
            ["size:    2K (2048 bytes)", "creator: ttxt", "comment: hi"]
        );
    }

    #[test]
    fn file_cells_show_the_icon_and_the_size_of_each_kind() {
        let file = |file_type: &[u8; 4], size| FileNameWithInfo {
            file_type: *file_type,
            creator: [0; 4],
            size,
            name: "a".into(),
        };
        assert_eq!(file_cells(&file(b"fldr", 3)), ["📁 a", "(3)"]);
        assert_eq!(file_cells(&file(b"alis", 3)), ["🔗 a", "-"]);
        assert_eq!(file_cells(&file(b"JPEG", 1536)), ["📷 a", "1K"]);
        assert_eq!(file_cells(&file(b"????", 512)), ["📄 a", "<1K"]);
    }

    #[test]
    fn push_tab_names_the_tab_of_a_chat_line_or_a_news_post() {
        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
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
