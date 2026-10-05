//! `hls`, a Hotline server daemon built on `hotline-server`.

mod accounts;
mod activity;
mod args;
mod banner;
mod bans;
mod file;
mod news;
mod policy;
mod tracker;

use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use hotline_server::{
    Account, AccountProvider, DefaultChatCommands, InMemoryFloodPolicy, NoBanner, Providers,
    Server, ServerConfig, UrlBannerProvider,
};

use accounts::{random_password, FileAccountProvider, ADMIN_LOGIN, GUEST_LOGIN};
use activity::{FileActivityLog, LogPaths};
use args::Args;
use banner::FileBannerProvider;
use bans::FileBanProvider;
use file::DiskFileProvider;
use news::{import_openline_news, FileFlatNewsProvider, FileThreadedNewsProvider};
use policy::HlsPolicy;
use tracker::{run_beacon, TrackerTarget};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    if let Some(dir) = &args.import_news {
        return import_news(dir, &args).await;
    }

    let config = ServerConfig {
        bind_addr: format!("{}:{}", args.bind, args.port).into(),
        agreement: args.agreement.into(),
        server_name: args.name.as_str().into(),
        console_prefix: args.console_prefix,
        max_connections: NonZeroUsize::new(args.max_connections),
        max_connections_per_address: NonZeroUsize::new(args.max_connections_per_address),
        max_downloads: args.max_downloads,
        max_downloads_per_client: args.max_downloads_per_client,
        encoding: args.encoding,
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
            .put(ADMIN_LOGIN.to_string(), Account::admin(password.as_str()))
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
    let log_paths = LogPaths {
        downloads: args.download_log.clone(),
        uploads: args.upload_log.clone(),
        connects: args.connect_log.clone(),
        accounts: args.account_log.clone(),
    };
    log_paths.check().await?;
    let activity = Arc::new(FileActivityLog::new(log_paths));
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
            activity: activity.clone(),
            policy: Arc::new(HlsPolicy),
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

    let result = tokio::select! {
        result = server.run() => result.map_err(anyhow::Error::from),
        result = shutdown_signal() => result.map(|()| tracing::info!("shutting down")),
    };
    activity.flush().await;
    result
}

/// Wait for Ctrl-C, or for SIGTERM on Unix.
async fn shutdown_signal() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}

/// Writes the news tree in the openline folder `dir` to `--news-tree-file`.
async fn import_news(dir: &Path, args: &Args) -> anyhow::Result<()> {
    let out = Path::new(&args.news_tree_file);
    anyhow::ensure!(
        !out.exists(),
        "{} already exists (--news-tree-file); move it away to import",
        out.display()
    );
    let tree = import_openline_news(dir, args.encoding)
        .with_context(|| format!("failed to import news from {}", dir.display()))?;
    tokio::fs::write(out, serde_json::to_vec_pretty(&tree)?)
        .await
        .with_context(|| format!("failed to write {}", out.display()))?;
    tracing::info!(from = %dir.display(), to = %out.display(), "imported openline news");
    Ok(())
}
