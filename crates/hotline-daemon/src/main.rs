mod accounts;
mod args;
mod banner;
mod bans;
mod file;
mod news;

use std::sync::Arc;

use clap::Parser;
use hotline_server::{InMemoryFloodPolicy, NoBanner, Server, ServerConfig, UrlBannerProvider};

use accounts::FileAccountProvider;
use args::Args;
use banner::FileBannerProvider;
use bans::FileBanProvider;
use file::DiskFileProvider;
use news::FileNewsProvider;

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
    let files = Arc::new(DiskFileProvider::open(args.files_dir, args.comments_file).await?);
    let news = Arc::new(FileNewsProvider::open(args.news_file, args.news_tree_file).await?);
    let bans = Arc::new(FileBanProvider::open(args.ban_file).await?);
    let flood = Arc::new(InMemoryFloodPolicy::default());
    let banner: Arc<dyn hotline_server::BannerProvider> = match args.banner {
        Some(value) if value.starts_with("http://") || value.starts_with("https://") => {
            Arc::new(UrlBannerProvider::new(value))
        }
        Some(path) => Arc::new(FileBannerProvider::open(path).await?),
        None => Arc::new(NoBanner),
    };
    Server::new(config, accounts, files, news, bans, flood, banner)
        .run()
        .await?;
    Ok(())
}
