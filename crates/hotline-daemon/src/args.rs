use clap::Parser;

/// A minimal Hotline server.
#[derive(Parser, Debug)]
#[command(name = "hls", version, about)]
pub struct Args {
    /// Port to listen on.
    #[arg(long, default_value_t = 5500)]
    pub port: u16,

    /// Interface to bind.
    #[arg(long, default_value = "0.0.0.0")]
    pub bind: String,

    /// Server name reported to clients on login.
    #[arg(long, default_value = "Hotline")]
    pub name: String,

    /// Agreement text shown to clients on login (empty disables it).
    #[arg(long, default_value = "Hotline")]
    pub agreement: String,

    /// Directory to store account files in (created if missing).
    #[arg(long, default_value = "accounts")]
    pub accounts_dir: String,

    /// Directory served by file listing/download/upload (created if
    /// missing). The transfer data connection listens on `port + 1`.
    #[arg(long, default_value = "files")]
    pub files_dir: String,

    /// File to persist file/folder comments to, as JSON (created if
    /// missing). Kept outside `files_dir` so it never shows up in a
    /// client's own file listing.
    #[arg(long, default_value = "comments.json")]
    pub comments_file: String,

    /// File to persist classic (flat) news to, as plain text (created if
    /// missing).
    #[arg(long, default_value = "news.txt")]
    pub news_file: String,

    /// File to persist threaded news to, as JSON (created if missing).
    #[arg(long, default_value = "news.json")]
    pub news_tree_file: String,

    /// File to persist address bans to, as JSON (created if missing).
    #[arg(long, default_value = "bans.json")]
    pub ban_file: String,

    /// Chat prefix for console commands, such as `/`; unset relays all chat.
    #[arg(long)]
    pub console_prefix: Option<char>,

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
    #[arg(long, default_value = "")]
    pub tracker_description: String,

    /// Banner shown to clients on login (unset disables the banner). An
    /// `http://`/`https://` value is shown as a URL; anything else is a
    /// local or relative path to an image file served over `HTXF`.
    #[arg(long)]
    pub banner: Option<String>,
}
