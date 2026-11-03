use std::path::PathBuf;

use clap::Parser;
use hotline_proto::text_encoding::{self, Encoding};

/// Terminal UI Hotline client.
#[derive(Parser, Debug)]
#[command(name = "hlc", version, about)]
pub struct Args {
    /// Server host.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Server port.
    #[arg(long, default_value_t = 5500)]
    pub port: u16,

    /// Account login (blank for guest).
    #[arg(long, default_value = "")]
    pub login: String,

    /// Account password. HLC_PASSWORD keeps it out of the process list.
    #[arg(long, env = "HLC_PASSWORD", hide_env_values = true, default_value = "")]
    pub password: String,

    /// The name in the user list.
    #[arg(long, default_value = "RustHL")]
    pub name: String,

    /// Icon ID.
    #[arg(long, default_value_t = 0)]
    pub icon: u16,

    /// Folder that downloads are saved in.
    #[arg(long, default_value = ".")]
    pub download_dir: PathBuf,

    /// Text encoding of the connection, as a WHATWG label such as
    /// `macintosh` or `utf-8`. It must match the server's.
    #[arg(long, default_value = "macintosh", value_parser = text_encoding::from_label)]
    pub encoding: &'static Encoding,

    /// Do not ring the terminal bell.
    #[arg(long)]
    pub no_bell: bool,

    /// List the servers a tracker knows about, then exit without
    /// connecting. Takes `host[:port]`; the port defaults to 5498.
    #[arg(long, value_name = "HOST[:PORT]")]
    pub tracker: Option<String>,
}
