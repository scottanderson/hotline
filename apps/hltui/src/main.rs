mod app;
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

use app::App;
use hotline_client::HotlineClient;

/// Terminal UI Hotline client.
#[derive(Parser, Debug)]
#[command(name = "hltui", version, about)]
struct Args {
    /// Server host.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Server port.
    #[arg(long, default_value_t = 5500)]
    port: u16,

    /// Account login (blank for guest).
    #[arg(long, default_value = "")]
    login: String,

    /// Account password.
    #[arg(long, default_value = "")]
    password: String,

    /// Display nickname.
    #[arg(long, default_value = "RustHL")]
    nick: String,

    /// Icon ID.
    #[arg(long, default_value_t = 0)]
    icon: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let addr = format!("{}:{}", args.host, args.port);
    let (client, mut events) = HotlineClient::connect(&addr).await?;
    client
        .login(&args.login, &args.password, &args.nick, args.icon)
        .await?;
    let users = client.get_user_name_list().await.unwrap_or_default();

    let mut app = App::new(args.nick, users);

    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal, &mut app, &client, &mut events).await;
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
    app: &mut App,
    client: &HotlineClient,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<hotline_proto::Transaction>,
) -> anyhow::Result<()> {
    let mut term_events = EventStream::new();

    terminal.draw(|f| ui::draw(f, app))?;
    while !app.should_quit {
        tokio::select! {
            // Server-initiated traffic: chat, user list changes, ...
            tx = events.recv() => {
                let Some(tx) = tx else { break }; // connection closed
                app.apply_server_event(&tx);
            }
            // Terminal input.
            ev = term_events.next() => {
                let Some(Ok(ev)) = ev else { break };
                if let Event::Key(key) = ev {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    match key.code {
                        KeyCode::Esc => app.quit(),
                        KeyCode::Enter => {
                            if let Some(line) = app.take_input() {
                                app::send_chat(app, client, line).await;
                            }
                        }
                        KeyCode::Backspace => app.on_backspace(),
                        KeyCode::Char('c')
                            if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            app.quit();
                        }
                        KeyCode::Char(c) => app.on_char(c),
                        _ => {}
                    }
                }
            }
        }
        terminal.draw(|f| ui::draw(f, app))?;
    }
    Ok(())
}
