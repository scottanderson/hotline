use std::path::PathBuf;

use clap::Parser;

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

    /// Display nickname.
    #[arg(long, default_value = "RustHL")]
    pub nick: String,

    /// Icon ID.
    #[arg(long, default_value_t = 0)]
    pub icon: u16,

    /// Folder that downloads are saved in.
    #[arg(long, default_value = ".")]
    pub download_dir: PathBuf,

    /// List the servers a tracker knows about, then exit without
    /// connecting. Takes `host[:port]`; the port defaults to 5498.
    #[arg(long, value_name = "HOST[:PORT]")]
    pub tracker: Option<String>,
}
