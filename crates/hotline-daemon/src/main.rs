mod accounts;
mod files;

use std::sync::Arc;

use clap::Parser;
use hotline_server::{Server, ServerConfig};

use accounts::FileAccountProvider;
use files::DirectoryFileProvider;

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
    let files = Arc::new(DirectoryFileProvider::open(args.files_dir).await?);
    Server::new(config, accounts, files).run().await?;
    Ok(())
}
