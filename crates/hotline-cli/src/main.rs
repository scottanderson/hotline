//! `hlc`, a terminal Hotline client built on `hotline-client`.

mod args;
mod line;
mod session;
mod tracker;
mod ui;

use clap::Parser;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use args::Args;
use hotline_client::HotlineClient;
use session::{ClientSession, EventOutcome, Mode};

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

    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal, &mut session, &client, &mut events).await;
    restore_terminal()?;
    result
}

fn setup_terminal() -> anyhow::Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    std::io::stdout().execute(EnterAlternateScreen)?;
    let terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    Ok(terminal)
}

fn restore_terminal() -> anyhow::Result<()> {
    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    session: &mut ClientSession,
    client: &HotlineClient,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<hotline_proto::Transaction>,
) -> anyhow::Result<()> {
    let mut term_events = EventStream::new();
    // The line being typed, not yet sent.
    let mut input = String::new();
    let mut should_quit = false;

    terminal.draw(|f| ui::draw(f, session, &input))?;
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
                if let Event::Key(key) = ev {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if matches!(session.mode, Mode::Agreement(_)) {
                        match key.code {
                            KeyCode::Esc => should_quit = true,
                            KeyCode::Enter => session.accept_agreement(client).await,
                            _ => {}
                        }
                        continue;
                    }
                    match key.code {
                        KeyCode::Esc => should_quit = true,
                        KeyCode::Enter => {
                            if !input.is_empty() {
                                let line = std::mem::take(&mut input);
                                session.send_chat(client, line).await;
                            }
                        }
                        KeyCode::Backspace => {
                            input.pop();
                        }
                        KeyCode::Char('c')
                            if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            should_quit = true;
                        }
                        KeyCode::Char(c) => input.push(c),
                        _ => {}
                    }
                }
            }
        }
        terminal.draw(|f| ui::draw(f, session, &input))?;
    }
    Ok(())
}
