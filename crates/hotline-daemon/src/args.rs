use std::path::PathBuf;

use clap::Parser;
use hotline_proto::text_encoding::{self, Encoding};
use hotline_server::{
    DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_CONNECTIONS_PER_ADDRESS, DEFAULT_MAX_DOWNLOADS,
    DEFAULT_MAX_DOWNLOADS_PER_CLIENT,
};

/// A minimal Hotline server.
#[derive(Parser, Debug)]
#[command(name = "hls", version, about)]
pub struct Args {
    /// Port to listen on. File transfers use the next port.
    #[arg(long, default_value_t = 5500)]
    pub port: u16,

    /// Interface to bind.
    #[arg(long, default_value = "0.0.0.0")]
    pub bind: String,

    /// Server name reported to clients on login.
    #[arg(long, default_value = "Hotline")]
    pub name: String,

    /// Text encoding of every connection, as a WHATWG label such as
    /// `macintosh` or `utf-8`.
    #[arg(long, default_value = "macintosh", value_parser = text_encoding::from_label)]
    pub encoding: &'static Encoding,

    /// Agreement text shown to clients on login (empty disables it).
    #[arg(long, default_value = "Hotline")]
    pub agreement: String,

    /// Directory to store account files in (created if missing).
    #[arg(long, default_value = "accounts")]
    pub accounts_dir: String,

    /// Directory that clients see (created if missing).
    #[arg(long, default_value = "files")]
    pub files_dir: String,

    /// File to persist classic (flat) news to, as plain text (created if
    /// missing).
    #[arg(long, default_value = "news.txt")]
    pub news_file: String,

    /// File to persist threaded news to, as JSON (created if missing).
    #[arg(long, default_value = "news.json")]
    pub news_tree_file: String,

    /// Import an openline `News` folder into `--news-tree-file`, then
    /// exit. The text in it uses `--encoding`.
    #[arg(long)]
    pub import_news: Option<PathBuf>,

    /// File to persist address bans to, as JSON (created if missing).
    #[arg(long, default_value = "bans.json")]
    pub ban_file: String,

    /// Chat prefix for console commands, such as `/`; unset relays all chat.
    #[arg(long)]
    pub console_prefix: Option<char>,

    /// Open connections before further ones are refused (0 means no limit).
    #[arg(long, default_value_t = DEFAULT_MAX_CONNECTIONS)]
    pub max_connections: usize,

    /// Open connections from one address before further ones are refused
    /// (0 means no limit).
    #[arg(long, default_value_t = DEFAULT_MAX_CONNECTIONS_PER_ADDRESS)]
    pub max_connections_per_address: usize,

    /// Concurrent downloads before further ones queue (0 uses the default).
    #[arg(long, default_value_t = DEFAULT_MAX_DOWNLOADS)]
    pub max_downloads: usize,

    /// Concurrent downloads for one user before further ones queue (0 uses
    /// the default).
    #[arg(long, default_value_t = DEFAULT_MAX_DOWNLOADS_PER_CLIENT)]
    pub max_downloads_per_client: usize,

    /// File to log each download to (unset logs nothing).
    #[arg(long)]
    pub download_log: Option<PathBuf>,

    /// File to log each upload to (unset logs nothing).
    #[arg(long)]
    pub upload_log: Option<PathBuf>,

    /// File to log each completed login to (unset logs nothing).
    #[arg(long)]
    pub connect_log: Option<PathBuf>,

    /// File to log each account change to (unset logs nothing).
    #[arg(long)]
    pub account_log: Option<PathBuf>,

    /// Tracker to register with, as `[password@]host[:port]`, repeatable.
    /// The port defaults to 5499. Unset disables registration.
    #[arg(long = "tracker", value_name = "[PASSWORD@]HOST[:PORT]")]
    pub trackers: Vec<String>,

    /// Tracker ID that keeps this server's listed name when its address
    /// changes. Unset uses the ID saved in `--tracker-id-file`.
    #[arg(long)]
    pub tracker_id: Option<u32>,

    /// File holding the saved tracker ID, created with a random one if missing.
    #[arg(long, default_value = "tracker-id")]
    pub tracker_id_file: String,

    /// Description sent to trackers alongside `--name`.
    #[arg(long, default_value = "", hide_default_value = true)]
    pub tracker_description: String,

    /// Banner shown to clients on login (unset disables the banner). An
    /// `http://`/`https://` value is shown as a URL; anything else is a
    /// local or relative path to an image file served over `HTXF`.
    #[arg(long)]
    pub banner: Option<String>,
}
