use std::path::PathBuf;

use clap::Parser;
use hotline_proto::text_encoding::{self, Encoding};

use crate::bell::Bell;
use crate::connect::{DEFAULT_PORT, Target};

/// Terminal UI Hotline client. A flag overrides the setting of the same
/// name in the settings file.
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
    #[arg(long)]
    pub name: Option<String>,

    /// Icon ID.
    #[arg(long)]
    pub icon: Option<u16>,

    /// Folder that downloads are saved in.
    #[arg(long, default_value = ".")]
    pub download_dir: PathBuf,

    /// Text encoding of the connection, as a WHATWG label such as
    /// `macintosh` or `utf-8`. It must match the server's.
    #[arg(long, default_value = "macintosh", value_parser = text_encoding::from_label)]
    pub encoding: &'static Encoding,

    /// Ring the terminal bell on the events of the settings file.
    #[arg(long, conflicts_with = "no_bell")]
    pub bell: bool,

    /// Do not ring the terminal bell.
    #[arg(long)]
    pub no_bell: bool,

    /// List the servers a tracker knows about, then exit without
    /// connecting. Takes `host[:port]`; the port defaults to 5498.
    #[arg(long, value_name = "HOST[:PORT]")]
    pub tracker: Option<String>,
}

impl Args {
    /// The events that ring the bell, given the events of the settings file.
    /// `--bell` with no events in the file gives [`Bell::DEFAULT`].
    pub const fn bell(&self, file: Bell) -> Bell {
        match (self.bell, self.no_bell) {
            (_, true) => Bell::empty(),
            (true, false) if file.is_empty() => Bell::DEFAULT,
            _ => file,
        }
    }

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
    fn bell_takes_the_events_of_the_file_unless_a_flag_turns_it_off() {
        let args = |flags: &[&str]| Args::parse_from([&["hlc"], flags].concat());
        assert_eq!(args(&[]).bell(Bell::ROOM), Bell::ROOM);
        assert_eq!(args(&[]).bell(Bell::empty()), Bell::empty());
        assert_eq!(args(&["--no-bell"]).bell(Bell::ROOM), Bell::empty());
        assert_eq!(args(&["--bell"]).bell(Bell::ROOM), Bell::ROOM);
        assert_eq!(args(&["--bell"]).bell(Bell::empty()), Bell::DEFAULT);
    }

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
