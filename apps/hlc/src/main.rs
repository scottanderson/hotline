use clap::Parser;
use hotline_client::HotlineClient;
use hotline_proto::ServerFieldId;
use tokio::io::{AsyncBufReadExt, BufReader};

/// A minimal Hotline CLI client: logs in, prints incoming chat, and lets
/// you type lines to send as public chat.
#[derive(Parser, Debug)]
#[command(name = "hlc", version, about)]
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
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let addr = format!("{}:{}", args.host, args.port);
    println!("connecting to {addr}...");
    let (client, mut events) = HotlineClient::connect(&addr).await?;

    let login_result = client
        .login(&args.login, &args.password, &args.nick, args.icon)
        .await?;
    println!(
        "logged in. server: {}",
        login_result
            .server_name
            .unwrap_or_else(|| "(unnamed)".into())
    );

    // Print server-initiated transactions (chat, user changes, ...) as
    // they arrive.
    tokio::spawn(async move {
        while let Some(tx) = events.recv().await {
            if let Some(msg) = tx.field(ServerFieldId::Text) {
                println!("{}", msg.as_string());
            } else {
                tracing::debug!(type_ = tx.type_, "event");
            }
        }
    });

    let files = client.get_file_name_list().await?;
    println!("root folder ({} items):", files.len());
    for f in &files {
        let kind = if f.is_folder() { "dir " } else { "file" };
        println!("  [{kind}] {}", f.name);
    }

    println!("type a message and press enter to chat, ctrl-d to quit");
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        if !line.is_empty() {
            client.send_chat(&line).await?;
        }
    }

    Ok(())
}
