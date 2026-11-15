//! `hlc`, a terminal Hotline client built on `hotline-client`.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

mod args;
mod bell;
mod connect;
mod file_edit;
mod line;
mod locale;
mod session;
mod tasks;
mod tracker;
mod transfer;
mod ui;
mod view;

use anyhow::Context;
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
use connect::{Connection, Target};
use file_edit::FileEdit;
use futures::future::{BoxFuture, OptionFuture};
use hotline_client::HotlineClient;
use hotline_proto::opcode::TransactionType;
use hotline_proto::text_encoding::{Encoding, EncodingHandle};
use hotline_proto::types::HlPath;
use hotline_proto::{Received, Transaction};
use session::{
    ClientSession, Disconnect, EventOutcome, Mode, NewsFolder, NewsTarget, TransferLine, KEEP_ALIVE,
};
use tasks::{Step, Task, TaskId, Tasks};
use tokio::sync::mpsc;
use transfer::Conflict;
use view::{AccountForm, Action, ConnectForm, Focus, Modal, OptionsForm, TextBox, View};

/// How often the task list shows new progress.
const TICK: std::time::Duration = std::time::Duration::from_millis(250);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    hotline_proto::types::HlDate::set_locale(locale::user_locale());

    if let Some(address) = &args.tracker {
        let list = tracker::query(address, hotline_proto::text_encoding::DEFAULT).await?;
        print!("{}", tracker::format_list(&list));
        return Ok(());
    }

    let target = args.target();
    let mut session = ClientSession::new(args.name, args.icon, EncodingHandle::default());
    session.download_dir = args.download_dir;
    let bell = if args.no_bell {
        Bell::empty()
    } else {
        Bell::DEFAULT
    };

    let (mut terminal, enhanced) = setup_terminal()?;
    let result = run(&mut terminal, &mut session, target, args.encoding, bell).await;
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
    start: Option<Target>,
    encoding: &'static Encoding,
    bell: Bell,
) -> anyhow::Result<()> {
    let mut term_events = EventStream::new();
    let mut view = View::default();
    // A terminal that does not report focus has it.
    let mut focused = true;
    let mut should_quit = false;
    // `true` while the screen lags behind the state.
    let mut stale = false;
    let (lines_tx, mut lines_rx) = mpsc::unbounded_channel();
    // The connection that logged in, and its pushes.
    let mut client: Option<HotlineClient> = None;
    let mut events: Option<mpsc::UnboundedReceiver<Transaction<Received>>> = None;
    let mut tasks = Tasks::default();
    // A new connection and its login, while they run.
    let mut connecting: Option<BoxFuture<'static, anyhow::Result<Connection>>> = None;
    // The server of the last connection, or of the one that runs.
    let mut target = start;
    match &target {
        Some(target) => connecting = Some(start_connect(target, session)),
        None => view.modal = Some(Modal::Connect(connect_form(None))),
    }
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_keep_alive = Instant::now();

    view.disconnected = true;
    terminal.draw(|f| ui::draw(f, session, &mut view))?;
    while !should_quit {
        let mut happened = Bell::empty();
        let keep_alive_at = keep_alive_due(client.as_ref(), last_keep_alive);
        // The reason that the connection ended.
        let mut ended: Option<Disconnect> = None;
        let keep_alive = tokio::time::sleep_until(keep_alive_at.into());
        let push = OptionFuture::from(events.as_mut().map(|events| events.recv()));
        let wake = tokio::select! {
            Some(line) = lines_rx.recv() => Wake::Transfer(line),
            Some(step) = tasks.next() => Wake::Reply(step),
            _ = tick.tick(), if tasks.is_busy() || !view.tasks.is_empty() => Wake::Tick,
            () = keep_alive, if client.is_some() => Wake::KeepAlive,
            Some(result) = OptionFuture::from(connecting.as_mut()) => Wake::Connected(result),
            Some(tx) = push => Wake::Push(tx),
            ev = term_events.next() => Wake::Terminal(ev),
        };
        match wake {
            Wake::Transfer(TransferLine::Started(line)) => session.lines.push(line),
            Wake::Transfer(TransferLine::Ended(line)) => {
                session.lines.push(line);
                happened = Bell::TRANSFER;
            }
            Wake::Reply(step) => step(session, &mut view, &mut tasks),
            Wake::Tick => {}
            Wake::KeepAlive => {
                if let Some(client) = &client {
                    if idle_since(client, last_keep_alive).elapsed() >= KEEP_ALIVE {
                        tasks.run(Task::unlisted(), session.keep_alive(client));
                        last_keep_alive = Instant::now();
                    }
                }
            }
            Wake::Connected(result) => {
                connecting = None;
                match result {
                    Ok(connection) => {
                        session.connected(connection.client.encoding(), connection.login.version);
                        client = Some(connection.client);
                        events = Some(connection.events);
                        tasks = Tasks::default();
                        last_keep_alive = Instant::now();
                    }
                    Err(e) => {
                        session.show_error(format!("{e:#}"));
                    }
                }
            }
            Wake::Push(None) => ended = Some(session.disconnect(Disconnect::Closed)),
            Wake::Push(Some(tx)) => {
                if let TransactionType::Push(opcode) = tx.r#type() {
                    happened = Bell::for_push(opcode);
                }
                if let Some(tab) = ui::push_tab(session, &tx) {
                    happened |= Bell::for_tab(tab, view.tab, focused);
                }
                match session.apply_server_event(&tx) {
                    EventOutcome::NeedsAgreementAccept => {
                        if let Some(client) = &client {
                            tasks.run(
                                Task::login("Logging in..."),
                                session.accept_agreement(client),
                            );
                        }
                    }
                    EventOutcome::Disconnected(reason) => ended = Some(reason),
                    EventOutcome::Applied => {}
                }
            }
            Wake::Terminal(Some(Ok(ev))) => {
                let action = match ev {
                    // A mouse move changes nothing.
                    Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved && !stale => {
                        continue
                    }
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if matches!(session.mode, Mode::Agreement(_)) {
                            match key.code {
                                KeyCode::Esc => should_quit = true,
                                KeyCode::Enter => {
                                    if let Some(client) = &client {
                                        let request = session.accept_agreement(client);
                                        tasks.run(Task::login("Logging in..."), request);
                                    }
                                }
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
                            view.side(),
                            view.has_sub_row(),
                        );
                        view.handle_mouse(mouse, &areas, users, files, Instant::now())
                    }
                    Event::FocusGained | Event::FocusLost => {
                        focused = matches!(ev, Event::FocusGained);
                        Action::None
                    }
                    _ => Action::None,
                };
                let flow = perform(
                    action,
                    client.as_ref(),
                    session,
                    &mut view,
                    &mut tasks,
                    &lines_tx,
                )
                .await;
                match flow {
                    Flow::Go => {}
                    Flow::Quit => should_quit = true,
                    Flow::OpenConnect => {
                        view.modal = Some(Modal::Connect(connect_form(target.as_ref())));
                    }
                    Flow::Connect {
                        host,
                        port,
                        login,
                        password,
                    } => {
                        if let Some(client) = client.take() {
                            events = None;
                            disconnect(&client, session, &mut tasks).await;
                        }
                        let new = Target {
                            host,
                            port,
                            login,
                            password,
                            encoding,
                        };
                        connecting = Some(start_connect(&new, session));
                        target = Some(new);
                    }
                    Flow::Reconnect => match &target {
                        Some(target) => connecting = Some(start_connect(target, session)),
                        None => view.modal = Some(Modal::Connect(connect_form(None))),
                    },
                    Flow::Disconnect => {
                        if let Some(client) = client.take() {
                            events = None;
                            disconnect(&client, session, &mut tasks).await;
                        }
                    }
                }
            }
            Wake::Terminal(_) => break,
        }
        if let Some(reason) = ended {
            if let Some(client) = client.take() {
                // The server may be gone already.
                let _ = client.close().await;
            }
            events = None;
            end_connection(session, &mut tasks);
            view.modal = Some(Modal::Disconnected(reason));
        }
        view.disconnected = client.is_none();
        view.set_tabs(ui::tab_labels(session));
        view.news_folder_count = session.news_folders.as_ref().map_or(0, Vec::len);
        view.news_count = session.news_threads.len();
        view.account_count = session.accounts.as_ref().map_or(0, Vec::len);
        view.access = session.granted();
        tasks.update(session);
        view.set_tasks(tasks.rows(session));
        view.note_revisions(ui::tab_revisions(session));
        bell.ring(happened)?;
        if view.modal.is_none() {
            view.modal = session.invites.pop_front().map(Modal::Invite);
        }
        // One frame follows a burst of input, such as a drag.
        stale = event::poll(Duration::ZERO).unwrap_or(false);
        if !stale {
            terminal.draw(|f| ui::draw(f, session, &mut view))?;
        }
    }
    Ok(())
}

/// What wakes the main loop.
enum Wake {
    /// A file transfer that starts or ends.
    Transfer(TransferLine),
    /// A reply to a request.
    Reply(Step),
    /// A moment while a task runs, to show its progress.
    Tick,
    /// No traffic or keep-alive attempt for a while.
    KeepAlive,
    /// A new connection that logged in, or failed.
    Connected(anyhow::Result<Connection>),
    /// Server-initiated traffic, such as chat and user list changes. `None`
    /// when the server closes the connection.
    Push(Option<Transaction<Received>>),
    /// Terminal input. `None` or an error when the input ends.
    Terminal(Option<std::io::Result<Event>>),
}

/// The later of the last traffic on `client` and the last keep-alive
/// attempt.
fn idle_since(client: &HotlineClient, last_keep_alive: Instant) -> Instant {
    client.last_traffic().max(last_keep_alive)
}

/// Connect to `target` and log in, and show that it starts.
fn start_connect(
    target: &Target,
    session: &mut ClientSession,
) -> BoxFuture<'static, anyhow::Result<Connection>> {
    let address = target.address();
    session.show_status(format!("connecting to {address}..."));
    let connecting = connect::connect(target.clone());
    Box::pin(async move {
        connecting
            .await
            .with_context(|| format!("failed to connect to {address}"))
    })
}

/// The Connect to form, filled with `target` if there is one.
fn connect_form(target: Option<&Target>) -> ConnectForm {
    match target {
        Some(target) => ConnectForm::new(&target.address(), &target.login, &target.password),
        None => ConnectForm::new("", "", ""),
    }
}

/// When the connection to `client` needs a keep-alive. With no connection,
/// one keep-alive period after `last_keep_alive`.
fn keep_alive_due(client: Option<&HotlineClient>, last_keep_alive: Instant) -> Instant {
    let idle = client.map_or(last_keep_alive, |client| {
        idle_since(client, last_keep_alive)
    });
    idle.checked_add(KEEP_ALIVE).unwrap_or(idle)
}

/// End the connection to `client`, and drop its state.
async fn disconnect(client: &HotlineClient, session: &mut ClientSession, tasks: &mut Tasks) {
    if let Err(e) = client.close().await {
        session.show_error(format!("failed to disconnect: {e}"));
    }
    session.show_status("disconnected");
    end_connection(session, tasks);
}

/// Drop the state of the connection that ended, and stop its tasks.
fn end_connection(session: &mut ClientSession, tasks: &mut Tasks) {
    session.clear();
    *tasks = Tasks::default();
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

/// What the loop does after an action.
enum Flow {
    Go,
    Quit,
    /// Log in again to the server of the last connection.
    Reconnect,
    /// End the connection, and stay open.
    Disconnect,
    /// Ask for a server to connect to.
    OpenConnect,
    /// End the connection, if any, and connect to `host`.
    Connect {
        host: String,
        port: u16,
        login: String,
        password: String,
    },
}

/// Run `action`. An action that sends a request shows an error when
/// there is no `client`. A transfer sends its lines to `lines`.
async fn perform(
    action: Action,
    client: Option<&HotlineClient>,
    session: &mut ClientSession,
    view: &mut View,
    tasks: &mut Tasks,
    lines: &mpsc::UnboundedSender<TransferLine>,
) -> Flow {
    match action {
        Action::None => {}
        Action::Quit => return Flow::Quit,
        Action::Reconnect => return Flow::Reconnect,
        Action::Disconnect => return Flow::Disconnect,
        Action::OpenConnect => return Flow::OpenConnect,
        Action::Connect {
            host,
            port,
            login,
            password,
        } => {
            return Flow::Connect {
                host,
                port,
                login,
                password,
            };
        }
        Action::EditOptions => {
            let form = OptionsForm::edit(&session.user_info(), session.queue_transfers);
            view.modal = Some(Modal::Options(form));
        }
        Action::SetOptions {
            info,
            queue_transfers,
        } => {
            session.queue_transfers = queue_transfers;
            match client {
                Some(client) => session.set_client_user_info(client, info).await,
                None => session.set_user_info(info),
            }
        }
        Action::StopLogin(id) => {
            if let TaskId::Request(id) = id {
                tasks.cancel(id);
            }
            return Flow::Disconnect;
        }
        action => match client {
            Some(client) => request(action, client, session, view, tasks, lines).await,
            None => session.show_error("not connected"),
        },
    }
    Flow::Go
}

/// Run `action`, which sends a request to `client`. A transfer sends its
/// lines to `lines`.
async fn request(
    action: Action,
    client: &HotlineClient,
    session: &mut ClientSession,
    view: &mut View,
    tasks: &mut Tasks,
    lines: &mpsc::UnboundedSender<TransferLine>,
) {
    match action {
        Action::SendChat(text) => session.send_chat(client, &text).await,
        Action::SendRoomChat { chat_id, text } => {
            session.send_room_chat(client, chat_id, &text).await;
        }
        Action::InviteNewChat(user_id) => {
            let request = ClientSession::invite_new_chat(client, user_id);
            tasks.start(Task::change("Opening private chat..."), request, show_room);
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
            let request = ClientSession::join_chat(client, chat_id);
            tasks.start(Task::change("Joining private chat..."), request, show_room);
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
            let request = ClientSession::send_instant_msg(client, user_id, &text);
            tasks.run(Task::change("Sending private message..."), request);
        }
        Action::GetClientInfoText(user_id) => {
            let request = ClientSession::get_client_info_text(client, user_id);
            let task = Task::read(format!("Get info for “{}”", session.user_name(user_id)));
            tasks.start(task, request, |info, _, view, _| {
                open(view, info.map(Modal::Info));
            });
        }
        Action::DisconnectUser { user_id, ban } => {
            let task = Task::change(format!("Disconnect user “{}”", session.user_name(user_id)));
            tasks.run(task, session.disconnect_user(client, user_id, ban));
        }
        Action::UserBroadcast(text) => {
            let request = ClientSession::user_broadcast(client, &text);
            tasks.run(Task::change("Broadcasting..."), request);
        }
        Action::GetMsgs => {
            tasks.run(
                Task::read("Transferring messages..."),
                ClientSession::get_msgs(client),
            );
        }
        Action::OldPostNews(text) => {
            let request = ClientSession::old_post_news(client, &text);
            tasks.run(Task::change("Posting message..."), request);
        }
        Action::ListFiles => list_files(client, tasks, session.file_path.clone()),
        Action::OpenFolder(name) => list_files(client, tasks, session.child_folder(&name)),
        Action::ParentFolder => {
            if let Some(path) = session.parent_folder() {
                list_files(client, tasks, path);
            }
        }
        Action::DownloadFile {
            name,
            save_as,
            start,
        } => {
            let task = Task::transfer(format!("Downloading “{name}”"));
            let request = session.download_file(client, &name, &save_as, start, lines);
            tasks.start(task, request, ask);
        }
        Action::DownloadFolder { name, start } => {
            let task = Task::transfer(format!("Downloading folder “{name}”"));
            tasks.start(
                task,
                session.download_folder(client, &name, start, lines),
                ask,
            );
        }
        Action::CancelTask(TaskId::Request(id)) => tasks.cancel(id),
        Action::StartTransfer(id) => session.start_transfer(id),
        Action::CancelTask(TaskId::Transfer(id)) => {
            tasks.forget(TaskId::Transfer(id));
            let request = session.cancel_transfer(client, id);
            tasks.run(Task::unlisted(), request);
        }
        Action::Upload { path, start } => {
            let path = std::path::Path::new(&path);
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let does = if path.is_dir() {
                "Uploading folder"
            } else {
                "Uploading"
            };
            let task = Task::transfer(format!("{does} “{name}”"));
            tasks.start(task, session.upload(client, path, start, lines), ask);
        }
        Action::AskFileEdit { edit, name } => match edit {
            FileEdit::Comment => {
                let task = Task::read(format!("Get info for file/folder “{name}”"));
                let request = session.get_file_info(client, &name);
                tasks.start(task, request, move |info, _, view, _| {
                    let input = info.map(|info| info.comment.into());
                    open(
                        view,
                        input.map(|input| Modal::FileEdit { edit, name, input }),
                    );
                });
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
            let task = Task::change(match edit {
                FileEdit::NewFolder => format!("Create folder “{text}”"),
                FileEdit::Rename | FileEdit::Comment => {
                    format!("Set info for file/folder “{name}”")
                }
                FileEdit::Move => format!("Move file/folder “{name}”"),
                FileEdit::Alias => format!("Make alias of “{name}”"),
            });
            tasks.run(task, session.edit_file(client, edit, &name, &text));
        }
        Action::DeleteFile(name) => {
            let kind = if session.lists_folder(&name) {
                "folder"
            } else {
                "file"
            };
            let task = Task::change(format!("Delete {kind} “{name}”"));
            tasks.run(task, session.delete_file(client, &name));
        }
        Action::GetFileInfo(name) => {
            let task = Task::read(format!("Get info for file/folder “{name}”"));
            tasks.start(
                task,
                session.get_file_info(client, &name),
                |info, _, view, _| {
                    open(view, info.map(Modal::FileInfo));
                },
            );
        }
        Action::ListThreadedNews => {
            tasks.start(
                Task::read("Listing root news bundle..."),
                session.list_threaded_news(client),
                {
                    let client = client.clone();
                    move |(), session, view, tasks| {
                        view.news_folders = TableState::default().with_selected(Some(0));
                        reselect_news_folder(&client, session, view, tasks);
                    }
                },
            );
        }
        Action::HighlightNewsFolder(index) => {
            view.news_folders.select(Some(index));
            reselect_news_folder(client, session, view, tasks);
        }
        Action::OpenNewsFolder(index) => {
            match session.news_folders.as_ref().and_then(|f| f.get(index)) {
                Some(node) if node.kind == NewsFolder::Category => view.focus = Focus::NewsThreads,
                Some(node) => {
                    let task = Task::read(format!("Listing news bundle “{}”", node.name()));
                    tasks.run(task, session.toggle_news_folder(client, index));
                }
                None => {}
            }
        }
        Action::ExpandNewsFolder(index) => {
            let name = session
                .news_name(NewsTarget::Folder(index))
                .unwrap_or_default();
            let task = Task::read(format!("Listing news bundle “{name}”"));
            tasks.run(task, session.expand_news_folder(client, index));
        }
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
                reselect_news_folder(client, session, view, tasks);
            }
        }
        Action::PreviewArticle(index) => preview_news_article(client, session, tasks, index),
        Action::OpenArticle(index) => {
            if let Some(summary) = session.news_threads.get(index).map(|t| t.summary.clone()) {
                let task = Task::read(format!("Getting news article “{}”", summary.title));
                let request = session.read_article(client, &summary);
                tasks.start(task, request, |article, _, view, _| {
                    open(view, article.map(Modal::Article));
                });
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
                Task::change(format!("Posting article {title}")),
                session.post_article(client, parent_id, &title, &text),
                {
                    let client = client.clone();
                    move |(), session, view, tasks| {
                        reselect_news_thread(&client, session, view, tasks);
                    }
                },
            );
        }
        Action::AskDeleteNews(target) => {
            if let NewsTarget::Folder(index) = target {
                if !session.may_delete_news_folder(index) {
                    return;
                }
            }
            view.modal = session.news_name(target).map(|name| Modal::DeleteNews {
                target,
                name: name.into(),
            });
        }
        Action::DeleteNewsItem(target) => {
            let name = session.news_name(target).unwrap_or_default();
            let label = match target {
                NewsTarget::Folder(index) => {
                    let folders = session.news_folders.as_deref().unwrap_or_default();
                    match folders.get(index) {
                        Some(node) => format!("Deleting news {} {name}", node.kind.label()),
                        None => format!("Deleting {name}"),
                    }
                }
                NewsTarget::Article(_) => format!("Deleting article {name}"),
            };
            tasks.start(
                Task::change(label),
                session.delete_news_item(client, target),
                {
                    let client = client.clone();
                    move |(), session, view, tasks| match target {
                        NewsTarget::Folder(_) => {
                            reselect_news_folder(&client, session, view, tasks)
                        }
                        NewsTarget::Article(_) => {
                            reselect_news_thread(&client, session, view, tasks);
                        }
                    }
                },
            );
        }
        Action::NewNewsItem { name, kind } => {
            let count = session.news_folders.as_ref().map_or(0, Vec::len);
            let index = view.news_folders.selected().filter(|&i| i < count);
            let path = index.and_then(|i| session.news_folder_path(i));
            let task = Task::change(match kind {
                NewsFolder::Bundle => format!("Create news bundle “{name}”"),
                NewsFolder::Category => format!("Create news category “{name}”"),
            });
            tasks.start(task, session.new_news_item(client, index, &name, kind), {
                let client = client.clone();
                move |(), session, view, tasks| {
                    if let Some(row) = path.and_then(|path| session.news_folder_row(&path)) {
                        view.news_folders.select(Some(row));
                    }
                    reselect_news_folder(&client, session, view, tasks);
                }
            });
        }
        Action::ListAccounts => {
            let task = Task::read("Transferring list of accounts...");
            tasks.start(task, session.list_accounts(client), |(), _, view, _| {
                view.accounts = ListState::default().with_selected(Some(0));
            });
        }
        Action::EditAccount(index) => {
            let account = session.accounts.as_ref().and_then(|a| a.get(index));
            view.modal = account.map(|a| Modal::Account(AccountForm::edit(a)));
        }
        Action::OpenAccount(login) => {
            let task = Task::read(format!("Opening account “{login}”"));
            let request = ClientSession::open_account(client, &login);
            tasks.start(task, request, |account, _, view, _| {
                open(view, account.map(|a| Modal::Account(AccountForm::edit(&a))));
            });
        }
        Action::SaveAccount { old_login, account } => {
            let task = Task::change(match &old_login {
                None => format!("Creating account “{}”", account.login),
                Some(old) => format!("Modifying account “{old}”"),
            });
            tasks.run(task, session.save_account(client, old_login, account));
        }
        Action::AskDeleteAccount(index) => {
            let account = session.accounts.as_ref().and_then(|a| a.get(index));
            view.modal = account.map(|a| Modal::DeleteAccount {
                login: a.login.clone(),
            });
        }
        Action::DeleteAccount(login) => {
            let task = Task::change(format!("Deleting account “{login}”"));
            tasks.run(task, session.delete_account(client, &login));
        }
        Action::None
        | Action::Quit
        | Action::Reconnect
        | Action::Disconnect
        | Action::OpenConnect
        | Action::Connect { .. }
        | Action::EditOptions
        | Action::SetOptions { .. }
        | Action::StopLogin(_) => {}
    }
}

/// List the folder `path`, and select its first item.
fn list_files(client: &HotlineClient, tasks: &mut Tasks, path: HlPath) {
    let task = Task::read(match path.components.last() {
        Some(name) => format!("Listing folder “{name}”"),
        None => "Listing root folder...".into(),
    });
    let request = ClientSession::get_file_name_list(client, path);
    tasks.start(task, request, |listed, _, view, _| {
        if listed {
            view.files = TableState::default().with_selected(Some(0));
        }
    });
}

/// Keep the tree selection in range, and show the threads of that row.
/// A new category selects its first thread.
fn reselect_news_folder(
    client: &HotlineClient,
    session: &mut ClientSession,
    view: &mut View,
    tasks: &mut Tasks,
) {
    let count = session.news_folders.as_ref().map_or(0, Vec::len);
    let index = view
        .news_folders
        .selected()
        .unwrap_or(0)
        .min(count.saturating_sub(1));
    view.news_folders.select(Some(index));
    let category = session.news_category.clone();
    let name = session
        .news_name(NewsTarget::Folder(index))
        .unwrap_or_default();
    let task = Task::read(format!("Listing news category “{name}”"));
    let request = session.highlight_news_folder(client, index);
    let client = client.clone();
    tasks.start(task, request, move |(), session, view, tasks| {
        if session.news_category != category || session.news_category.is_none() {
            view.news = TableState::default().with_selected(Some(0));
            view.preview_scroll = 0;
            preview_news_article(&client, session, tasks, 0);
        }
    });
}

/// Keep the thread selection in range, and show that article in the preview.
fn reselect_news_thread(
    client: &HotlineClient,
    session: &mut ClientSession,
    view: &mut View,
    tasks: &mut Tasks,
) {
    let index = view
        .news
        .selected()
        .unwrap_or(0)
        .min(session.news_threads.len().saturating_sub(1));
    view.news.select(Some(index));
    preview_news_article(client, session, tasks, index);
}

/// Show thread `index` in the preview, and scroll to the top for a new article.
fn preview_news_article(
    client: &HotlineClient,
    session: &mut ClientSession,
    tasks: &mut Tasks,
    index: usize,
) {
    let shown = session.news_preview.as_ref().map(|article| article.id);
    let title = session
        .news_threads
        .get(index)
        .map(|t| t.summary.title.clone());
    let task = Task::read(format!(
        "Getting news article “{}”",
        title.unwrap_or_default()
    ));
    let request = session.preview_article(client, index);
    tasks.start(task, request, move |(), session, view, _| {
        if session.news_preview.as_ref().map(|article| article.id) != shown {
            view.preview_scroll = 0;
        }
    });
}
