mod accounts;
mod bans;
mod files;
mod news;

use std::sync::Arc;

use clap::Parser;
use hotline_server::{InMemoryFloodPolicy, Server, ServerConfig};

use accounts::FileAccountProvider;
use bans::FileBanProvider;
use files::FileFileProvider;
use news::FileNewsProvider;

/// A minimal Hotline server.
#[derive(Parser, Debug)]
#[command(name = "hls", version, about)]
struct Args {
    /// Port to listen on.
    #[arg(long, default_value_t = 5500)]
    port: u16,

    /// Interface to bind.
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,

    /// Server name reported to clients on login.
    #[arg(long, default_value = "Hotline")]
    name: String,

    /// Agreement text shown to clients on login (empty disables it).
    #[arg(long, default_value = "Hotline")]
    agreement: String,

    /// Directory to store account files in (created if missing).
    #[arg(long, default_value = "accounts")]
    accounts_dir: String,

    /// Directory served by file listing/download/upload (created if
    /// missing). The transfer data connection listens on `port + 1`.
    #[arg(long, default_value = "files")]
    files_dir: String,

    /// File to persist file/folder comments to, as JSON (created if
    /// missing). Kept outside `files_dir` so it never shows up in a
    /// client's own file listing.
    #[arg(long, default_value = "comments.json")]
    comments_file: String,

    /// File to persist classic (flat) news to, as plain text (created if
    /// missing).
    #[arg(long, default_value = "news.txt")]
    news_file: String,

    /// File to persist threaded news to, as JSON (created if missing).
    #[arg(long, default_value = "news.json")]
    news_tree_file: String,

    /// File to persist address bans to, as JSON (created if missing).
    #[arg(long, default_value = "bans.json")]
    ban_file: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let config = ServerConfig {
        bind_addr: format!("{}:{}", args.bind, args.port),
        agreement: args.agreement,
        server_name: args.name,
    };

    let accounts = Arc::new(FileAccountProvider::open(args.accounts_dir).await?);
    let files = Arc::new(FileFileProvider::open(args.files_dir, args.comments_file).await?);
    let news = Arc::new(FileNewsProvider::open(args.news_file, args.news_tree_file).await?);
    let bans = Arc::new(FileBanProvider::open(args.ban_file).await?);
    let flood = Arc::new(InMemoryFloodPolicy::default());
    Server::new(config, accounts, files, news, bans, flood)
        .run()
        .await?;
    Ok(())
}
