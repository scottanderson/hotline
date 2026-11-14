use std::path::PathBuf;

use clap::Parser;
use hotline_proto::text_encoding::{self, Encoding};

use crate::connect::{Target, DEFAULT_PORT};

/// Terminal UI Hotline client.
#[derive(Parser, Debug)]
#[command(name = "hlc", version, about)]
pub struct Args {
    /// Server host. Without it, `hlc` starts without a connection.
    #[arg(long)]
    pub host: Option<String>,

    /// Server port, 5500 by default.
    #[arg(long, requires = "host")]
    pub port: Option<u16>,

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

impl Args {
    /// The server to connect to at the start, if `--host` names one.
    pub fn target(&self) -> Option<Target> {
        self.host.as_ref().map(|host| Target {
            host: host.clone(),
            port: self.port.unwrap_or(DEFAULT_PORT),
            login: self.login.clone(),
            password: self.password.clone(),
            encoding: self.encoding,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_takes_the_host_and_its_login() {
        let args = Args::parse_from(["hlc", "--host", "example.com", "--login", "alice"]);
        let target = args.target().unwrap();
        assert_eq!(
            (target.address(), target.login.as_str()),
            ("example.com:5500".into(), "alice")
        );
        let args = Args::parse_from(["hlc", "--host", "example.com", "--port", "6000"]);
        assert_eq!(args.target().unwrap().address(), "example.com:6000");
        assert!(Args::parse_from(["hlc"]).target().is_none());
    }

    #[test]
    fn port_needs_a_host() {
        assert!(Args::try_parse_from(["hlc", "--port", "6000"]).is_err());
    }
}
