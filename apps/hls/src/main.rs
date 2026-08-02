use clap::Parser;
use hotline_server::{Server, ServerConfig};

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
    #[arg(long, default_value = "hotline-rs")]
    name: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let config = ServerConfig {
        bind_addr: format!("{}:{}", args.bind, args.port),
        agreement: String::new(),
        server_name: args.name,
        ..ServerConfig::default()
    };

    Server::new(config).run().await?;
    Ok(())
}
