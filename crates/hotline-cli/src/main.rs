//! `hlc`, a terminal Hotline client built on `hotline-client`.

mod args;
mod file_edit;
mod line;
mod locale;
mod session;
mod tracker;
mod transfer;
mod ui;
mod view;

use clap::Parser;
use std::time::Instant;

use futures::StreamExt;
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::terminal::supports_keyboard_enhancement;
use ratatui::crossterm::ExecutableCommand;
use ratatui::widgets::{ListState, TableState};
use ratatui::DefaultTerminal;

use args::Args;
use file_edit::FileEdit;
use hotline_client::HotlineClient;
use hotline_proto::types::HlPath;
use line::Line;
use session::{ClientSession, EventOutcome, Mode, NewsFolder, NewsTarget, KEEP_ALIVE};
use tokio::sync::mpsc;
use view::{AccountForm, Action, Focus, Modal, OptionsForm, View};

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

    let mut session = ClientSession::new(args.nick, args.icon, client.encoding());
    session.server_version = login.version;
    session.download_dir = args.download_dir;

    let (mut terminal, enhanced) = setup_terminal()?;
    let result = run(&mut terminal, &mut session, &client, &mut events).await;
    restore_terminal(enhanced)?;
    result
}

/// Set up the terminal with [`ratatui::try_init`], mouse capture, and the
/// keyboard enhancement flags when the terminal has them. The `bool` is
/// `true` if the terminal now reports Shift+Enter apart from Enter.
/// A panic on any thread restores the terminal, prints, and ends `hlc`.
fn setup_terminal() -> anyhow::Result<(DefaultTerminal, bool)> {
    let terminal = ratatui::try_init()?;
    std::io::stdout().execute(EnableMouseCapture)?;
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

/// Turn off mouse capture and the keyboard enhancement flags.
fn restore_input(enhanced: bool) -> std::io::Result<()> {
    if enhanced {
        std::io::stdout().execute(PopKeyboardEnhancementFlags)?;
    }
    std::io::stdout().execute(DisableMouseCapture)?;
    Ok(())
}

async fn run(
    terminal: &mut DefaultTerminal,
    session: &mut ClientSession,
    client: &HotlineClient,
    events: &mut mpsc::UnboundedReceiver<hotline_proto::Transaction<hotline_proto::Received>>,
) -> anyhow::Result<()> {
    let mut term_events = EventStream::new();
    let mut view = View::default();
    let mut should_quit = false;
    let (done_tx, mut done_rx) = mpsc::unbounded_channel();
    let mut last_keep_alive = Instant::now();
    let idle_since = |last_keep_alive| client.last_traffic().max(last_keep_alive);

    terminal.draw(|f| ui::draw(f, session, &mut view))?;
    while !should_quit {
        tokio::select! {
            // A finished file transfer.
            Some(line) = done_rx.recv() => session.lines.push(line),
            // No traffic or keep-alive attempt for a while.
            () = tokio::time::sleep_until((idle_since(last_keep_alive) + KEEP_ALIVE).into()) => {
                if idle_since(last_keep_alive).elapsed() >= KEEP_ALIVE {
                    session.keep_alive(client).await;
                    last_keep_alive = Instant::now();
                }
            }
            // Server-initiated traffic: chat, user list changes, ...
            tx = events.recv() => {
                let Some(tx) = tx else { break }; // connection closed
                match session.apply_server_event(&tx) {
                    EventOutcome::NeedsAgreementAccept => session.accept_agreement(client).await,
                    EventOutcome::Quit => should_quit = true,
                    EventOutcome::Applied => {}
                }
            }
            // Terminal input.
            ev = term_events.next() => {
                let Some(Ok(ev)) = ev else { break };
                let action = match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if matches!(session.mode, Mode::Agreement(_)) {
                            match key.code {
                                KeyCode::Esc => should_quit = true,
                                KeyCode::Enter => session.accept_agreement(client).await,
                                _ => {}
                            }
                            continue;
                        }
                        let users = ui::tab_users(session, view.tab);
                        let files = session.files.as_deref().unwrap_or_default();
                        view.handle_key(key, users, files)
                    }
                    Event::Mouse(mouse) if !matches!(session.mode, Mode::Agreement(_)) => {
                        let areas = ui::areas(terminal.size()?.into(), view.input(), &view.hints(), view.shows_users());
                        let users = ui::tab_users(session, view.tab);
                        let files = session.files.as_deref().unwrap_or_default();
                        view.handle_mouse(mouse, &areas, users, files, Instant::now())
                    }
                    _ => Action::None,
                };
                should_quit = perform(action, session, &mut view, client, &done_tx).await;
            }
        }
        view.set_tabs(ui::tab_labels(session));
        view.news_folder_count = session.news_folders.as_ref().map_or(0, Vec::len);
        view.news_count = session.news_threads.len();
        view.account_count = session.accounts.as_ref().map_or(0, Vec::len);
        view.can_broadcast = session.can_broadcast();
        view.can_disconnect = session.can_disconnect();
        view.note_revisions(ui::tab_revisions(session));
        if view.modal.is_none() {
            view.modal = session.invites.pop_front().map(Modal::Invite);
        }
        terminal.draw(|f| ui::draw(f, session, &mut view))?;
    }
    Ok(())
}

/// Run `action`. A finished transfer sends its line to `done`. `true` means quit.
async fn perform(
    action: Action,
    session: &mut ClientSession,
    view: &mut View,
    client: &HotlineClient,
    done: &mpsc::UnboundedSender<Line>,
) -> bool {
    match action {
        Action::None => {}
        Action::Quit => return true,
        Action::SendChat(text) => session.send_chat(client, &text).await,
        Action::SendRoomChat { chat_id, text } => {
            session.send_room_chat(client, chat_id, &text).await;
        }
        Action::InviteNewChat(user_id) => {
            if let Some(chat_id) = session.invite_new_chat(client, user_id).await {
                view.set_tabs(ui::tab_labels(session));
                view.show_room(chat_id);
            }
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
            if let Some(chat_id) = session.join_chat(client, chat_id).await {
                view.set_tabs(ui::tab_labels(session));
                view.show_room(chat_id);
            }
        }
        Action::RejectChatInvite(chat_id) => session.reject_chat_invite(client, chat_id).await,
        Action::LeaveChat(chat_id) => session.leave_chat(client, chat_id).await,
        Action::EditChatSubject(chat_id) => {
            let input = session
                .room(chat_id)
                .map(|room| room.subject.to_string())
                .unwrap_or_default();
            view.modal = Some(Modal::Subject { chat_id, input });
        }
        Action::SetChatSubject { chat_id, subject } => {
            session.set_chat_subject(client, chat_id, &subject).await;
        }
        Action::SendInstantMsg { user_id, text } => {
            session.send_instant_msg(client, user_id, &text).await;
        }
        Action::GetClientInfoText(user_id) => {
            view.modal = session
                .get_client_info_text(client, user_id)
                .await
                .map(Modal::Info);
        }
        Action::DisconnectUser { user_id, ban } => {
            session.disconnect_user(client, user_id, ban).await;
        }
        Action::UserBroadcast(text) => session.user_broadcast(client, &text).await,
        Action::EditOptions => {
            view.modal = Some(Modal::Options(OptionsForm::edit(&session.user_info())));
        }
        Action::SetClientUserInfo(info) => session.set_client_user_info(client, info).await,
        Action::GetMsgs => session.get_msgs(client).await,
        Action::OldPostNews(text) => session.old_post_news(client, &text).await,
        Action::ListFiles => {
            let path = session.file_path.clone();
            list_files(session, view, client, path).await;
        }
        Action::OpenFolder(name) => {
            let path = session.child_folder(&name);
            list_files(session, view, client, path).await;
        }
        Action::ParentFolder => {
            if let Some(path) = session.parent_folder() {
                list_files(session, view, client, path).await;
            }
        }
        Action::DownloadFile {
            name,
            save_as,
            start,
        } => {
            let conflict = session
                .download_file(client, &name, &save_as, start, done)
                .await;
            view.modal = conflict.map(Modal::ask);
        }
        Action::DownloadFolder { name, start } => {
            let conflict = session.download_folder(client, &name, start, done).await;
            view.modal = conflict.map(Modal::ask);
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
        Action::KillDownload(id) => session.kill_download(client, id).await,
        Action::Upload { path, start } => {
            let path = std::path::Path::new(&path);
            let conflict = session.upload(client, path, start, done).await;
            view.modal = conflict.map(Modal::ask);
        }
        Action::AskFileEdit { edit, name } => {
            let input = match edit {
                FileEdit::NewFolder => Some(String::new()),
                FileEdit::Rename => Some(name.to_string()),
                FileEdit::Comment => session
                    .get_file_info(client, &name)
                    .await
                    .map(|info| info.comment.into()),
                FileEdit::Move | FileEdit::Alias => Some(session.folder_text()),
            };
            view.modal = input.map(|input| Modal::FileEdit { edit, name, input });
        }
        Action::EditFile { edit, name, text } => {
            session.edit_file(client, edit, &name, &text).await;
        }
        Action::DeleteFile(name) => session.delete_file(client, &name).await,
        Action::GetFileInfo(name) => {
            view.modal = session
                .get_file_info(client, &name)
                .await
                .map(Modal::FileInfo);
        }
        Action::ListThreadedNews => {
            session.list_threaded_news(client).await;
            view.news_folders = TableState::default().with_selected(Some(0));
            reselect_news_folder(session, view, client).await;
        }
        Action::HighlightNewsFolder(index) => {
            view.news_folders.select(Some(index));
            reselect_news_folder(session, view, client).await;
        }
        Action::OpenNewsFolder(index) => {
            match session.news_folders.as_ref().and_then(|f| f.get(index)) {
                Some(node) if node.kind == NewsFolder::Category => view.focus = Focus::NewsThreads,
                Some(_) => session.toggle_news_folder(client, index).await,
                None => {}
            }
        }
        Action::ExpandNewsFolder(index) => session.expand_news_folder(client, index).await,
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
                reselect_news_folder(session, view, client).await;
            }
        }
        Action::PreviewArticle(index) => preview_news_article(session, view, client, index).await,
        Action::OpenArticle(index) => {
            if let Some(summary) = session.news_threads.get(index).map(|t| t.summary.clone()) {
                view.modal = session
                    .read_article(client, &summary)
                    .await
                    .map(Modal::Article);
            }
        }
        Action::NewPost => {
            if session.can_post() {
                view.modal = Some(Modal::Post {
                    parent_id: 0,
                    title: String::new(),
                    body: String::new(),
                    on_body: false,
                });
            }
        }
        Action::PostArticle {
            parent_id,
            title,
            text,
        } => {
            session.post_article(client, parent_id, &title, &text).await;
            reselect_news_thread(session, view, client).await;
        }
        Action::AskDeleteNews(target) => {
            view.modal = session.news_name(target).map(|name| Modal::DeleteNews {
                target,
                name: name.into(),
            });
        }
        Action::DeleteNewsItem(target) => {
            session.delete_news_item(client, target).await;
            match target {
                NewsTarget::Folder(_) => reselect_news_folder(session, view, client).await,
                NewsTarget::Article(_) => reselect_news_thread(session, view, client).await,
            }
        }
        Action::NewNewsItem { name, kind } => {
            let count = session.news_folders.as_ref().map_or(0, Vec::len);
            let index = view.news_folders.selected().filter(|&i| i < count);
            let path = index.and_then(|i| session.news_folder_path(i));
            session.new_news_item(client, index, &name, kind).await;
            if let Some(row) = path.and_then(|path| session.news_folder_row(&path)) {
                view.news_folders.select(Some(row));
            }
            reselect_news_folder(session, view, client).await;
        }
        Action::ListAccounts => {
            session.list_accounts(client).await;
            view.accounts = ListState::default().with_selected(Some(0));
        }
        Action::EditAccount(index) => {
            let account = session.accounts.as_ref().and_then(|a| a.get(index));
            view.modal = account.map(|a| Modal::Account(AccountForm::edit(a)));
        }
        Action::OpenAccount(login) => {
            let account = session.open_account(client, &login).await;
            view.modal = account.map(|a| Modal::Account(AccountForm::edit(&a)));
        }
        Action::SaveAccount { old_login, account } => {
            session.save_account(client, old_login, account).await;
        }
        Action::AskDeleteAccount(index) => {
            let account = session.accounts.as_ref().and_then(|a| a.get(index));
            view.modal = account.map(|a| Modal::DeleteAccount {
                login: a.login.clone(),
            });
        }
        Action::DeleteAccount(login) => session.delete_account(client, &login).await,
    }
    false
}

async fn list_files(
    session: &mut ClientSession,
    view: &mut View,
    client: &HotlineClient,
    path: HlPath,
) {
    if session.get_file_name_list(client, path).await {
        view.files = ListState::default().with_selected(Some(0));
    }
}

/// Keep the tree selection in range, and show the threads of that row.
/// A new category selects its first thread.
async fn reselect_news_folder(
    session: &mut ClientSession,
    view: &mut View,
    client: &HotlineClient,
) {
    let count = session.news_folders.as_ref().map_or(0, Vec::len);
    let index = view
        .news_folders
        .selected()
        .unwrap_or(0)
        .min(count.saturating_sub(1));
    view.news_folders.select(Some(index));
    let category = session.news_category.clone();
    session.highlight_news_folder(client, index).await;
    if session.news_category != category || session.news_category.is_none() {
        view.news = TableState::default().with_selected(Some(0));
        view.preview_scroll = 0;
        session.preview_article(client, 0).await;
    }
}

/// Keep the thread selection in range, and show that article in the preview.
async fn reselect_news_thread(
    session: &mut ClientSession,
    view: &mut View,
    client: &HotlineClient,
) {
    let index = view
        .news
        .selected()
        .unwrap_or(0)
        .min(session.news_threads.len().saturating_sub(1));
    view.news.select(Some(index));
    preview_news_article(session, view, client, index).await;
}

/// Show thread `index` in the preview, and scroll to the top for a new article.
async fn preview_news_article(
    session: &mut ClientSession,
    view: &mut View,
    client: &HotlineClient,
    index: usize,
) {
    let shown = session.news_preview.as_ref().map(|article| article.id);
    session.preview_article(client, index).await;
    if session.news_preview.as_ref().map(|article| article.id) != shown {
        view.preview_scroll = 0;
    }
}
