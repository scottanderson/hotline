use std::path::PathBuf;

use clap::{ArgAction, Parser};
use hotline_proto::text_encoding::{self, Encoding};
use hotline_server::{
    DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_CONNECTIONS_PER_ADDRESS, DEFAULT_MAX_DOWNLOADS,
    DEFAULT_MAX_DOWNLOADS_PER_CLIENT,
};
use tracing_subscriber::filter::LevelFilter;

use crate::fork_store::ForkStoreKind;

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

    /// Where to keep each file's Finder info, comment, and resource fork.
    #[arg(long, value_enum, default_value_t = ForkStoreKind::AppleDouble)]
    pub fork_store: ForkStoreKind,

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

    /// Log more: `-v` adds debug lines and `-vv` trace lines.
    #[arg(short, long, action = ArgAction::Count, conflicts_with = "quiet")]
    pub verbose: u8,

    /// Log less: `-q` logs warnings, `-qq` only errors, and `-qqq` nothing.
    #[arg(short, long, action = ArgAction::Count)]
    pub quiet: u8,
}

/// The most bytes of agreement text that a client gets.
const MAX_AGREEMENT: usize = 64 * 1024;

impl Args {
    /// The agreement text with CR line endings, cut to 64K in `encoding`.
    pub fn agreement(&self) -> Box<str> {
        let text = text_encoding::to_wire_lines(&self.agreement);
        text_encoding::cut(self.encoding, &text, MAX_AGREEMENT).into()
    }

    /// The log level that `-v` and `-q` set, with INFO as the default.
    pub fn log_level(&self) -> LevelFilter {
        match i16::from(self.verbose) - i16::from(self.quiet) {
            ..=-3 => LevelFilter::OFF,
            -2 => LevelFilter::ERROR,
            -1 => LevelFilter::WARN,
            0 => LevelFilter::INFO,
            1 => LevelFilter::DEBUG,
            2.. => LevelFilter::TRACE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(agreement: &str) -> Args {
        Args::parse_from(["hls", "--agreement", agreement])
    }

    #[test]
    fn agreement_changes_each_line_ending_to_cr() {
        assert_eq!(&*args("a\r\nb\nc\rd").agreement(), "a\rb\rc\rd");
    }

    #[test]
    fn agreement_is_cut_to_64k() {
        let long = "x".repeat(MAX_AGREEMENT + 1);
        assert_eq!(args(&long).agreement().len(), MAX_AGREEMENT);
    }

    fn log_level_of(flags: &[&str]) -> LevelFilter {
        let argv = ["hls"].iter().chain(flags);
        Args::parse_from(argv).log_level()
    }

    #[test]
    fn log_level_is_info_by_default() {
        assert_eq!(log_level_of(&[]), LevelFilter::INFO);
    }

    #[test]
    fn log_level_rises_with_each_verbose() {
        assert_eq!(log_level_of(&["-v"]), LevelFilter::DEBUG);
        assert_eq!(log_level_of(&["-vv"]), LevelFilter::TRACE);
        assert_eq!(log_level_of(&["-vvv"]), LevelFilter::TRACE);
    }

    #[test]
    fn log_level_falls_with_each_quiet() {
        assert_eq!(log_level_of(&["-q"]), LevelFilter::WARN);
        assert_eq!(log_level_of(&["--quiet", "--quiet"]), LevelFilter::ERROR);
        assert_eq!(log_level_of(&["-qqq"]), LevelFilter::OFF);
        assert_eq!(log_level_of(&["-qqqq"]), LevelFilter::OFF);
    }

    #[test]
    fn log_level_refuses_verbose_with_quiet() {
        assert!(Args::try_parse_from(["hls", "-v", "-q"]).is_err());
        assert!(Args::try_parse_from(["hls", "-q", "-v"]).is_err());
    }
}
