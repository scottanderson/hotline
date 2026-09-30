//! `hlc`, a terminal Hotline client built on `hotline-client`.

mod args;
mod line;
mod session;
mod tracker;
mod ui;
mod view;

use clap::Parser;
use std::time::Instant;

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use args::Args;
use hotline_client::HotlineClient;
use session::{ClientSession, EventOutcome, Mode};
use view::{Action, Modal, View};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    if let Some(address) = &args.tracker {
        let list = tracker::query(address, hotline_proto::text_encoding::DEFAULT).await?;
        print!("{}", tracker::format_list(&list));
        return Ok(());
    }

    let addr = format!("{}:{}", args.host, args.port);
    let (client, mut events) = HotlineClient::connect(&addr).await?;
    client.login(&args.login, &args.password).await?;

    let mut session = ClientSession::new(args.nick, args.icon, client.encoding());

    let (mut terminal, enhanced) = setup_terminal()?;
    let result = run(&mut terminal, &mut session, &client, &mut events).await;
    restore_terminal(enhanced)?;
    result
}

/// Also `true` if the terminal now reports Shift+Enter apart from Enter.
fn setup_terminal() -> anyhow::Result<(Terminal<CrosstermBackend<std::io::Stdout>>, bool)> {
    enable_raw_mode()?;
    std::io::stdout()
        .execute(EnterAlternateScreen)?
        .execute(EnableMouseCapture)?;
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        std::io::stdout().execute(PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
        ))?;
    }
    let terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    Ok((terminal, enhanced))
}

fn restore_terminal(enhanced: bool) -> anyhow::Result<()> {
    if enhanced {
        std::io::stdout().execute(PopKeyboardEnhancementFlags)?;
    }
    disable_raw_mode()?;
    std::io::stdout()
        .execute(DisableMouseCapture)?
        .execute(LeaveAlternateScreen)?;
    Ok(())
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    session: &mut ClientSession,
    client: &HotlineClient,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<hotline_proto::Transaction>,
) -> anyhow::Result<()> {
    let mut term_events = EventStream::new();
    let mut view = View::default();
    let mut should_quit = false;

    terminal.draw(|f| ui::draw(f, session, &mut view))?;
    while !should_quit {
        tokio::select! {
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
                        view.handle_key(key, &session.users)
                    }
                    Event::Mouse(mouse) if !matches!(session.mode, Mode::Agreement(_)) => {
                        let areas = ui::areas(terminal.size()?, &view.input);
                        view.handle_mouse(mouse, &areas, &session.users, Instant::now())
                    }
                    _ => Action::None,
                };
                should_quit = perform(action, session, &mut view, client).await;
            }
        }
        terminal.draw(|f| ui::draw(f, session, &mut view))?;
    }
    Ok(())
}

/// Run `action`. `true` means quit.
async fn perform(
    action: Action,
    session: &mut ClientSession,
    view: &mut View,
    client: &HotlineClient,
) -> bool {
    match action {
        Action::None => {}
        Action::Quit => return true,
        Action::SendChat(text) => session.send_chat(client, &text).await,
        Action::SendInstantMsg { user_id, text } => {
            session.send_instant_msg(client, user_id, &text).await;
        }
        Action::GetClientInfoText(user_id) => {
            if let Some(info) = session.get_client_info_text(client, user_id).await {
                view.modal = Some(Modal::Info {
                    name: info.name,
                    text: info.text,
                });
            }
        }
    }
    false
}
