//! `hls`, a Hotline server daemon built on `hotline-server`.

mod accounts;
mod args;
mod banner;
mod bans;
mod file;
mod news;

use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use hotline_server::{
    DefaultChatCommands, InMemoryFloodPolicy, NoBanner, Server, ServerConfig, UrlBannerProvider,
};

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
        console_prefix: args.console_prefix,
    };

    let accounts = Arc::new(
        FileAccountProvider::open(args.accounts_dir.clone())
            .await
            .with_context(|| {
                format!(
                    "failed to open accounts directory {} (--accounts-dir)",
                    args.accounts_dir
                )
            })?,
    );
    let files = Arc::new(
        DiskFileProvider::open(args.files_dir.clone(), args.comments_file.clone())
            .await
            .with_context(|| {
                format!(
                    "failed to open the file store (--files-dir {}, --comments-file {})",
                    args.files_dir, args.comments_file
                )
            })?,
    );
    let news = Arc::new(
        FileNewsProvider::open(args.news_file.clone(), args.news_tree_file.clone())
            .await
            .with_context(|| {
                format!(
                    "failed to open the news store (--news-file {}, --news-tree-file {})",
                    args.news_file, args.news_tree_file
                )
            })?,
    );
    let bans = Arc::new(
        FileBanProvider::open(args.ban_file.clone())
            .await
            .with_context(|| format!("failed to open ban file {} (--ban-file)", args.ban_file))?,
    );
    let flood = Arc::new(InMemoryFloodPolicy::default());
    let banner: Arc<dyn hotline_server::BannerProvider> = match args.banner {
        Some(value) if value.starts_with("http://") || value.starts_with("https://") => {
            Arc::new(UrlBannerProvider::new(value))
        }
        Some(path) => Arc::new(
            FileBannerProvider::open(path.clone())
                .await
                .with_context(|| format!("failed to open banner file {path} (--banner)"))?,
        ),
        None => Arc::new(NoBanner),
    };
    let commands = Arc::new(DefaultChatCommands);
    Server::new(config, accounts, files, news, bans, flood, banner, commands)
        .run()
        .await?;
    Ok(())
}
