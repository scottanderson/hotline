//! `hlc`, a terminal Hotline client built on `hotline-client`.

mod args;
mod bell;
mod file_edit;
mod line;
mod locale;
mod session;
mod tracker;
mod transfer;
mod ui;
mod view;

use clap::Parser;
use std::time::{Duration, Instant};

use futures::StreamExt;
use ratatui::crossterm::event::{
    self, DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEventKind, KeyboardEnhancementFlags, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::terminal::supports_keyboard_enhancement;
use ratatui::crossterm::ExecutableCommand;
use ratatui::widgets::{ListState, TableState};
use ratatui::DefaultTerminal;

use args::Args;
use bell::Bell;
use file_edit::FileEdit;
use hotline_client::HotlineClient;
use hotline_proto::opcode::TransactionType;
use hotline_proto::types::HlPath;
use line::Line;
use session::{
    ClientSession, Disconnect, EventOutcome, Mode, NewsFolder, NewsTarget, Next, Request,
    KEEP_ALIVE,
};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use transfer::Conflict;
use view::{AccountForm, Action, Focus, Modal, OptionsForm, TextBox, View};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    hotline_proto::types::HlDate::set_locale(locale::user_locale());

    if let Some(address) = &args.tracker {
        let list = tracker::query(address, hotline_proto::text_encoding::DEFAULT).await?;
        print!("{}", tracker::format_list(&list));
        return Ok(());
    }

    let addr = format!("{}:{}", args.host, args.port);
    let (client, mut events) = HotlineClient::connect(&addr).await?;
    client.encoding().set(args.encoding);
    let login = client.login(&args.login, &args.password).await?;

    let mut session = ClientSession::new(args.name, args.icon, client.encoding());
    session.server_version = login.version;
    session.download_dir = args.download_dir;
    let bell = if args.no_bell {
        Bell::empty()
    } else {
        Bell::DEFAULT
    };

    let (mut terminal, enhanced) = setup_terminal()?;
    let result = run(&mut terminal, &mut session, &client, &mut events, bell).await;
    restore_terminal(enhanced)?;
    result
}

/// Set up the terminal with [`ratatui::try_init`], mouse capture, focus
/// reports, and the keyboard enhancement flags when the terminal has them.
/// The `bool` is `true` if the terminal now reports Shift+Enter apart from
/// Enter.
/// A panic on any thread restores the terminal, prints, and ends `hlc`.
fn setup_terminal() -> anyhow::Result<(DefaultTerminal, bool)> {
    let terminal = ratatui::try_init()?;
    std::io::stdout().execute(EnableMouseCapture)?;
    std::io::stdout().execute(EnableFocusChange)?;
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        std::io::stdout().execute(PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
        ))?;
    }
    let ratatui_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_input(enhanced);
        ratatui_hook(info);
        std::process::exit(101);
    }));
    Ok((terminal, enhanced))
}

/// Undo [`setup_terminal`].
fn restore_terminal(enhanced: bool) -> anyhow::Result<()> {
    restore_input(enhanced)?;
    ratatui::try_restore()?;
    Ok(())
}

/// Turn off mouse capture, focus reports, and the keyboard enhancement flags.
fn restore_input(enhanced: bool) -> std::io::Result<()> {
    if enhanced {
        std::io::stdout().execute(PopKeyboardEnhancementFlags)?;
    }
    std::io::stdout().execute(DisableFocusChange)?;
    std::io::stdout().execute(DisableMouseCapture)?;
    Ok(())
}

async fn run(
    terminal: &mut DefaultTerminal,
    session: &mut ClientSession,
    client: &HotlineClient,
    events: &mut mpsc::UnboundedReceiver<hotline_proto::Transaction<hotline_proto::Received>>,
    bell: Bell,
) -> anyhow::Result<()> {
    let mut term_events = EventStream::new();
    let mut view = View::default();
    // A terminal that does not report focus has it.
    let mut focused = true;
    let mut should_quit = false;
    // The reason that the connection ended.
    let mut disconnected: Option<Disconnect> = None;
    // `true` while the screen lags behind the state.
    let mut stale = false;
    let (done_tx, mut done_rx) = mpsc::unbounded_channel();
    let mut tasks = Tasks::new(client);
    let mut last_keep_alive = Instant::now();
    let idle_since = |last_keep_alive| client.last_traffic().max(last_keep_alive);

    terminal.draw(|f| ui::draw(f, session, &mut view))?;
    while !should_quit {
        let was_connected = disconnected.is_none();
        let mut happened = Bell::empty();
        tokio::select! {
            // A finished file transfer.
            Some(line) = done_rx.recv() => {
                session.lines.push(line);
                happened = Bell::TRANSFER;
            }
            // A reply to a request.
            Some(step) = tasks.next() => step(session, &mut view, &mut tasks),
            // No traffic or keep-alive attempt for a while.
            () = tokio::time::sleep_until((idle_since(last_keep_alive) + KEEP_ALIVE).into()),
                if disconnected.is_none() =>
            {
                if idle_since(last_keep_alive).elapsed() >= KEEP_ALIVE {
                    tasks.run(session.keep_alive(client));
                    last_keep_alive = Instant::now();
                }
            }
            // Server-initiated traffic: chat, user list changes, ...
            tx = events.recv(), if disconnected.is_none() => match tx {
                None => disconnected = Some(session.disconnect(Disconnect::Closed)),
                Some(tx) => {
                    if let TransactionType::Push(opcode) = tx.r#type() {
                        happened = Bell::for_push(opcode);
                    }
                    if let Some(tab) = ui::push_tab(session, &tx) {
                        happened |= Bell::for_tab(tab, view.tab, focused);
                    }
                    match session.apply_server_event(&tx) {
                        EventOutcome::NeedsAgreementAccept => {
                            tasks.run(session.accept_agreement(client));
                        }
                        EventOutcome::Disconnected(reason) => disconnected = Some(reason),
                        EventOutcome::Applied => {}
                    }
                }
            },
            // Terminal input.
            ev = term_events.next() => {
                let Some(Ok(ev)) = ev else { break };
                let action = match ev {
                    // A mouse move changes nothing.
                    Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved && !stale => continue,
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if matches!(session.mode, Mode::Agreement(_)) {
                            match key.code {
                                KeyCode::Esc => should_quit = true,
                                KeyCode::Enter => tasks.run(session.accept_agreement(client)),
                                _ => {}
                            }
                            continue;
                        }
                        let users = ui::tab_users(session, view.tab);
                        let files = session.files.as_deref().unwrap_or_default();
                        view.handle_key(key, users, files)
                    }
                    Event::Mouse(mouse) if !matches!(session.mode, Mode::Agreement(_)) => {
                        let users = ui::tab_users(session, view.tab);
                        let files = session.files.as_deref().unwrap_or_default();
                        let areas = ui::areas(
                            terminal.size()?.into(),
                            view.input().map(TextBox::as_str),
                            &view.hints(files),
                            view.shows_users(),
                            view.has_room_row(),
                        );
                        view.handle_mouse(mouse, &areas, users, files, Instant::now())
                    }
                    Event::FocusGained | Event::FocusLost => {
                        focused = matches!(ev, Event::FocusGained);
                        Action::None
                    }
                    _ => Action::None,
                };
                should_quit = perform(action, session, &mut view, &mut tasks, &done_tx).await;
            }
        }
        view.set_tabs(ui::tab_labels(session));
        view.news_folder_count = session.news_folders.as_ref().map_or(0, Vec::len);
        view.news_count = session.news_threads.len();
        view.account_count = session.accounts.as_ref().map_or(0, Vec::len);
        view.access = session.granted();
        view.note_revisions(ui::tab_revisions(session));
        bell.ring(happened)?;
        // The end of the connection shows over any other modal.
        if view.modal.is_none() || was_connected && disconnected.is_some() {
            view.modal = match &disconnected {
                Some(reason) => Some(Modal::Disconnected(reason.clone())),
                None => session.invites.pop_front().map(Modal::Invite),
            };
        }
        // One frame follows a burst of input, such as a drag.
        stale = event::poll(Duration::ZERO).unwrap_or(false);
        if !stale {
            terminal.draw(|f| ui::draw(f, session, &mut view))?;
        }
    }
    Ok(())
}

/// What a reply does to the session and the view.
type Step = Box<dyn FnOnce(&mut ClientSession, &mut View, &mut Tasks) + Send>;

/// The requests that run off the event loop.
struct Tasks {
    set: JoinSet<Step>,
    client: HotlineClient,
}

impl Tasks {
    fn new(client: &HotlineClient) -> Self {
        Self {
            set: JoinSet::new(),
            client: client.clone(),
        }
    }

    /// Run `request`, then give its value to `then`.
    fn start<T: Send + 'static>(
        &mut self,
        request: Request<T>,
        then: impl FnOnce(T, &mut ClientSession, &mut View, &mut Self) + Send + 'static,
    ) {
        self.set.spawn(async move {
            let apply = request.reply().await;
            let step: Step = Box::new(move |session, view, tasks| match apply(session) {
                Next::Done(value) => then(value, session, view, tasks),
                Next::Then(request) => tasks.start(request, then),
            });
            step
        });
    }

    /// Run `request`.
    fn run(&mut self, request: Request<()>) {
        self.start(request, |(), _, _, _| {});
    }

    /// The step of the next reply. `None` when no request runs.
    async fn next(&mut self) -> Option<Step> {
        loop {
            match self.set.join_next().await? {
                Ok(step) => return Some(step),
                Err(e) if e.is_cancelled() => {}
                Err(e) => std::panic::resume_unwind(e.into_panic()),
            }
        }
    }
}

/// Show `modal`, if any.
fn open(view: &mut View, modal: Option<Modal>) {
    if modal.is_some() {
        view.modal = modal;
    }
}

/// Show the room `chat_id`, if any.
fn show_room(chat_id: Option<u32>, session: &mut ClientSession, view: &mut View, _: &mut Tasks) {
    if let Some(chat_id) = chat_id {
        view.set_tabs(ui::tab_labels(session));
        view.show_room(chat_id);
    }
}

/// Show the conflict that stops a transfer, if any.
fn ask(conflict: Option<Conflict>, _: &mut ClientSession, view: &mut View, _: &mut Tasks) {
    open(view, conflict.map(Modal::ask));
}

/// Run `action`. A finished transfer sends its line to `done`. `true` means quit.
async fn perform(
    action: Action,
    session: &mut ClientSession,
    view: &mut View,
    tasks: &mut Tasks,
    done: &mpsc::UnboundedSender<Line>,
) -> bool {
    let client = &tasks.client.clone();
    match action {
        Action::None => {}
        Action::Quit => return true,
        Action::SendChat(text) => session.send_chat(client, &text).await,
        Action::SendRoomChat { chat_id, text } => {
            session.send_room_chat(client, chat_id, &text).await;
        }
        Action::InviteNewChat(user_id) => {
            tasks.start(ClientSession::invite_new_chat(client, user_id), show_room);
        }
        Action::PickInvitee(chat_id) => {
            let members = ui::tab_users(session, view::Tab::Room(chat_id));
            let users = session
                .users
                .iter()
                .filter(|u| !members.iter().any(|m| m.user_id == u.user_id))
                .map(|u| (u.user_id, u.name.clone()))
                .collect();
            view.modal = Some(Modal::PickUser {
                chat_id,
                users,
                selected: 0,
            });
        }
        Action::InviteToChat { chat_id, user_id } => {
            session.invite_to_chat(client, chat_id, user_id).await;
        }
        Action::JoinChat(chat_id) => {
            tasks.start(ClientSession::join_chat(client, chat_id), show_room)
        }
        Action::RejectChatInvite(chat_id) => session.reject_chat_invite(client, chat_id).await,
        Action::LeaveChat(chat_id) => session.leave_chat(client, chat_id).await,
        Action::EditChatSubject(chat_id) => {
            let input = session
                .room(chat_id)
                .map(|room| room.subject.to_string())
                .unwrap_or_default();
            view.modal = Some(Modal::Subject {
                chat_id,
                input: input.into(),
            });
        }
        Action::SetChatSubject { chat_id, subject } => {
            session.set_chat_subject(client, chat_id, &subject).await;
        }
        Action::SendInstantMsg { user_id, text } => {
            tasks.run(ClientSession::send_instant_msg(client, user_id, &text));
        }
        Action::GetClientInfoText(user_id) => {
            let request = ClientSession::get_client_info_text(client, user_id);
            tasks.start(request, |info, _, view, _| {
                open(view, info.map(Modal::Info))
            });
        }
        Action::DisconnectUser { user_id, ban } => {
            tasks.run(session.disconnect_user(client, user_id, ban));
        }
        Action::UserBroadcast(text) => tasks.run(ClientSession::user_broadcast(client, &text)),
        Action::EditOptions => {
            view.modal = Some(Modal::Options(OptionsForm::edit(&session.user_info())));
        }
        Action::SetClientUserInfo(info) => session.set_client_user_info(client, info).await,
        Action::GetMsgs => tasks.run(ClientSession::get_msgs(client)),
        Action::OldPostNews(text) => tasks.run(ClientSession::old_post_news(client, &text)),
        Action::ListFiles => list_files(tasks, session.file_path.clone()),
        Action::OpenFolder(name) => list_files(tasks, session.child_folder(&name)),
        Action::ParentFolder => {
            if let Some(path) = session.parent_folder() {
                list_files(tasks, path);
            }
        }
        Action::DownloadFile {
            name,
            save_as,
            start,
        } => {
            tasks.start(
                session.download_file(client, &name, &save_as, start, done),
                ask,
            );
        }
        Action::DownloadFolder { name, start } => {
            tasks.start(session.download_folder(client, &name, start, done), ask);
        }
        Action::PickDownload => {
            let downloads = session.running_downloads();
            if !downloads.is_empty() {
                view.modal = Some(Modal::PickDownload {
                    downloads,
                    selected: 0,
                });
            }
        }
        Action::KillDownload(id) => tasks.run(session.kill_download(client, id)),
        Action::Upload { path, start } => {
            let path = std::path::Path::new(&path);
            tasks.start(session.upload(client, path, start, done), ask);
        }
        Action::AskFileEdit { edit, name } => match edit {
            FileEdit::Comment => {
                tasks.start(
                    session.get_file_info(client, &name),
                    move |info, _, view, _| {
                        let input = info.map(|info| info.comment.into());
                        open(
                            view,
                            input.map(|input| Modal::FileEdit { edit, name, input }),
                        );
                    },
                );
            }
            FileEdit::NewFolder | FileEdit::Rename | FileEdit::Move | FileEdit::Alias => {
                let input = match edit {
                    FileEdit::Rename => name.to_string(),
                    FileEdit::Move | FileEdit::Alias => session.folder_text(),
                    _ => String::new(),
                };
                view.modal = Some(Modal::FileEdit {
                    edit,
                    name,
                    input: input.into(),
                });
            }
        },
        Action::EditFile { edit, name, text } => {
            tasks.run(session.edit_file(client, edit, &name, &text));
        }
        Action::DeleteFile(name) => tasks.run(session.delete_file(client, &name)),
        Action::GetFileInfo(name) => {
            tasks.start(session.get_file_info(client, &name), |info, _, view, _| {
                open(view, info.map(Modal::FileInfo));
            });
        }
        Action::ListThreadedNews => {
            tasks.start(
                session.list_threaded_news(client),
                |(), session, view, tasks| {
                    view.news_folders = TableState::default().with_selected(Some(0));
                    reselect_news_folder(session, view, tasks);
                },
            );
        }
        Action::HighlightNewsFolder(index) => {
            view.news_folders.select(Some(index));
            reselect_news_folder(session, view, tasks);
        }
        Action::OpenNewsFolder(index) => {
            match session.news_folders.as_ref().and_then(|f| f.get(index)) {
                Some(node) if node.kind == NewsFolder::Category => view.focus = Focus::NewsThreads,
                Some(_) => tasks.run(session.toggle_news_folder(client, index)),
                None => {}
            }
        }
        Action::ExpandNewsFolder(index) => tasks.run(session.expand_news_folder(client, index)),
        Action::CollapseNewsFolder(index) => {
            let expanded = session
                .news_folders
                .as_ref()
                .and_then(|f| f.get(index))
                .is_some_and(|node| node.expanded);
            if expanded {
                session.collapse_news_folder(index);
            } else if let Some(parent) = session.news_folder_parent(index) {
                view.news_folders.select(Some(parent));
                reselect_news_folder(session, view, tasks);
            }
        }
        Action::PreviewArticle(index) => preview_news_article(session, tasks, index),
        Action::OpenArticle(index) => {
            if let Some(summary) = session.news_threads.get(index).map(|t| t.summary.clone()) {
                tasks.start(
                    session.read_article(client, &summary),
                    |article, _, view, _| {
                        open(view, article.map(Modal::Article));
                    },
                );
            }
        }
        Action::NewPost => {
            if session.can_post() {
                view.modal = Some(Modal::Post {
                    parent_id: 0,
                    title: TextBox::new(),
                    body: TextBox::new(),
                    on_body: false,
                });
            }
        }
        Action::PostArticle {
            parent_id,
            title,
            text,
        } => {
            tasks.start(
                session.post_article(client, parent_id, &title, &text),
                |(), session, view, tasks| reselect_news_thread(session, view, tasks),
            );
        }
        Action::AskDeleteNews(target) => {
            if let NewsTarget::Folder(index) = target {
                if !session.may_delete_news_folder(index) {
                    return false;
                }
            }
            view.modal = session.news_name(target).map(|name| Modal::DeleteNews {
                target,
                name: name.into(),
            });
        }
        Action::DeleteNewsItem(target) => {
            tasks.start(
                session.delete_news_item(client, target),
                move |(), session, view, tasks| match target {
                    NewsTarget::Folder(_) => reselect_news_folder(session, view, tasks),
                    NewsTarget::Article(_) => reselect_news_thread(session, view, tasks),
                },
            );
        }
        Action::NewNewsItem { name, kind } => {
            let count = session.news_folders.as_ref().map_or(0, Vec::len);
            let index = view.news_folders.selected().filter(|&i| i < count);
            let path = index.and_then(|i| session.news_folder_path(i));
            tasks.start(
                session.new_news_item(client, index, &name, kind),
                move |(), session, view, tasks| {
                    if let Some(row) = path.and_then(|path| session.news_folder_row(&path)) {
                        view.news_folders.select(Some(row));
                    }
                    reselect_news_folder(session, view, tasks);
                },
            );
        }
        Action::ListAccounts => {
            tasks.start(session.list_accounts(client), |(), _, view, _| {
                view.accounts = ListState::default().with_selected(Some(0));
            });
        }
        Action::EditAccount(index) => {
            let account = session.accounts.as_ref().and_then(|a| a.get(index));
            view.modal = account.map(|a| Modal::Account(AccountForm::edit(a)));
        }
        Action::OpenAccount(login) => {
            tasks.start(
                ClientSession::open_account(client, &login),
                |account, _, view, _| {
                    open(view, account.map(|a| Modal::Account(AccountForm::edit(&a))));
                },
            );
        }
        Action::SaveAccount { old_login, account } => {
            tasks.run(session.save_account(client, old_login, account));
        }
        Action::AskDeleteAccount(index) => {
            let account = session.accounts.as_ref().and_then(|a| a.get(index));
            view.modal = account.map(|a| Modal::DeleteAccount {
                login: a.login.clone(),
            });
        }
        Action::DeleteAccount(login) => tasks.run(session.delete_account(client, &login)),
    }
    false
}

/// List the folder `path`, and select its first item.
fn list_files(tasks: &mut Tasks, path: HlPath) {
    let request = ClientSession::get_file_name_list(&tasks.client, path);
    tasks.start(request, |listed, _, view, _| {
        if listed {
            view.files = ListState::default().with_selected(Some(0));
        }
    });
}

/// Keep the tree selection in range, and show the threads of that row.
/// A new category selects its first thread.
fn reselect_news_folder(session: &mut ClientSession, view: &mut View, tasks: &mut Tasks) {
    let count = session.news_folders.as_ref().map_or(0, Vec::len);
    let index = view
        .news_folders
        .selected()
        .unwrap_or(0)
        .min(count.saturating_sub(1));
    view.news_folders.select(Some(index));
    let category = session.news_category.clone();
    let request = session.highlight_news_folder(&tasks.client, index);
    tasks.start(request, move |(), session, view, tasks| {
        if session.news_category != category || session.news_category.is_none() {
            view.news = TableState::default().with_selected(Some(0));
            view.preview_scroll = 0;
            tasks.run(session.preview_article(&tasks.client, 0));
        }
    });
}

/// Keep the thread selection in range, and show that article in the preview.
fn reselect_news_thread(session: &mut ClientSession, view: &mut View, tasks: &mut Tasks) {
    let index = view
        .news
        .selected()
        .unwrap_or(0)
        .min(session.news_threads.len().saturating_sub(1));
    view.news.select(Some(index));
    preview_news_article(session, tasks, index);
}

/// Show thread `index` in the preview, and scroll to the top for a new article.
fn preview_news_article(session: &mut ClientSession, tasks: &mut Tasks, index: usize) {
    let shown = session.news_preview.as_ref().map(|article| article.id);
    let request = session.preview_article(&tasks.client, index);
    tasks.start(request, move |(), session, view, _| {
        if session.news_preview.as_ref().map(|article| article.id) != shown {
            view.preview_scroll = 0;
        }
    });
}
