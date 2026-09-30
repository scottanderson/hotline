//! `hls`, a Hotline server daemon built on `hotline-server`.

mod accounts;
mod args;
mod banner;
mod bans;
mod file;
mod news;
mod tracker;

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use hotline_server::{
    Account, AccountProvider, DefaultChatCommands, InMemoryFloodPolicy, NoBanner, Providers,
    Server, ServerConfig, UrlBannerProvider,
};

use accounts::{random_password, FileAccountProvider, ADMIN_LOGIN, GUEST_LOGIN};
use args::Args;
use banner::FileBannerProvider;
use bans::FileBanProvider;
use file::DiskFileProvider;
use news::{FileFlatNewsProvider, FileThreadedNewsProvider};
use tracker::{run_beacon, TrackerTarget};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let config = ServerConfig {
        bind_addr: format!("{}:{}", args.bind, args.port),
        agreement: args.agreement,
        server_name: args.name.clone(),
        console_prefix: args.console_prefix,
        max_connections: args.max_connections,
        max_connections_per_address: args.max_connections_per_address,
        ..ServerConfig::default()
    };

    let accounts = FileAccountProvider::open(args.accounts_dir.clone())
        .await
        .with_context(|| {
            format!(
                "failed to open accounts directory {} (--accounts-dir)",
                args.accounts_dir
            )
        })?;
    if accounts.is_empty().await {
        let password = random_password()?;
        accounts
            .put(ADMIN_LOGIN.to_string(), Account::admin(&password))
            .await
            .map_err(anyhow::Error::msg)?;
        println!(
            "created account \"{ADMIN_LOGIN}\" with password \"{password}\" in {}",
            args.accounts_dir
        );
        accounts
            .put(GUEST_LOGIN.to_string(), Account::guest())
            .await
            .map_err(anyhow::Error::msg)?;
    }
    let accounts = Arc::new(accounts);
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
    let flat_news = Arc::new(
        FileFlatNewsProvider::open(args.news_file.clone())
            .await
            .with_context(|| {
                format!(
                    "failed to open the flat news file {} (--news-file)",
                    args.news_file
                )
            })?,
    );
    let threaded_news = Arc::new(
        FileThreadedNewsProvider::open(args.news_tree_file.clone())
            .await
            .with_context(|| {
                format!(
                    "failed to open the news tree file {} (--news-tree-file)",
                    args.news_tree_file
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
    let server = Server::new(
        config,
        Providers {
            accounts,
            files,
            flat_news,
            threaded_news,
            bans,
            flood,
            banner,
            commands,
        },
    );

    if !args.trackers.is_empty() {
        let targets: Vec<TrackerTarget> = args
            .trackers
            .iter()
            .map(String::as_str)
            .map(TrackerTarget::parse)
            .collect();
        tracker::check(&targets, &args.name, &args.tracker_description)?;
        let pass_id = match args.tracker_id {
            Some(pass_id) => pass_id,
            None => tracker::load_pass_id(Path::new(&args.tracker_id_file)).await?,
        };
        tracing::info!(count = targets.len(), "registering with trackers");
        let state = server.state().clone();
        tokio::spawn(async move {
            if let Err(error) = run_beacon(
                state,
                targets,
                args.port,
                args.name,
                args.tracker_description,
                pass_id,
            )
            .await
            {
                tracing::error!(%error, "tracker registration stopped");
            }
        });
    }

    server.run().await?;
    Ok(())
}
