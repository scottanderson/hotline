//! The settings file of `hlc`, `$XDG_CONFIG_HOME/hlc/config.toml`.

use std::borrow::Cow;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::Context;
use hotline_proto::text_encoding::Encoding;
use hotline_proto::types::UserChangeOptions;
use serde::{Deserialize, Serialize};

use crate::bell::Bell;
use crate::connect::{Target, parse_address};

mod edit;

/// The settings that `hlc` keeps from one run to the next. A command-line
/// flag overrides a setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The name in the user list. `None` until the user gives one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<Box<str>>,
    pub icon: u16,
    /// `true` if each transfer waits until each older one ends.
    pub queue_transfers: bool,
    /// `true` to answer a private message with `automatic_response_text`.
    pub automatic_response: bool,
    /// The reply to a private message while `automatic_response` is on.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub automatic_response_text: Box<str>,
    /// `true` to refuse private messages.
    pub refuse_pm: bool,
    /// `true` to refuse private chat invitations.
    pub refuse_chat: bool,
    /// The events that ring the bell.
    pub bell: Bell,
    /// The name of the bookmark that `hlc` connects to when it starts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_connect: Option<Box<str>>,
    /// The saved servers, in the order of the server list.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<Bookmark>,
}

/// A saved server, and the login on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bookmark {
    /// The name, or `None` for [`Bookmark::default_name`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Box<str>>,
    /// `host` or `host:port`.
    pub address: Box<str>,
    #[serde(default, skip_serializing_if = "str::is_empty")]
    pub login: Box<str>,
    /// The password as the user typed it.
    #[serde(default, skip_serializing_if = "str::is_empty")]
    pub password: Box<str>,
}

impl Bookmark {
    /// The name of a bookmark that has no name of its own: `login@address`,
    /// or `address` for an empty login.
    pub fn default_name<'a>(login: &'_ str, address: &'a str) -> Cow<'a, str> {
        match login {
            "" => address.into(),
            login => format!("{login}@{address}").into(),
        }
    }

    /// The name of the bookmark. An empty name gives
    /// [`Bookmark::default_name`].
    pub fn name(&self) -> Cow<'_, str> {
        match self.name.as_deref() {
            None | Some("") => Self::default_name(&self.login, &self.address),
            Some(name) => name.into(),
        }
    }

    /// The server and the login of the bookmark, with `encoding`. `None`
    /// when [`parse_address`] refuses the address.
    pub fn target(&self, encoding: &'static Encoding) -> Option<Target> {
        let (host, port) = parse_address(&self.address)?;
        Some(Target {
            name: Some(self.name().into()),
            host,
            port,
            login: self.login.to_string(),
            password: self.password.to_string(),
            encoding,
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: None,
            icon: 414,
            queue_transfers: true,
            automatic_response: false,
            automatic_response_text: Box::default(),
            refuse_pm: false,
            refuse_chat: false,
            bell: Bell::DEFAULT,
            auto_connect: None,
            bookmarks: Vec::new(),
        }
    }
}

impl Config {
    /// Save `bookmark`, in place of the bookmark of the same name if there
    /// is one.
    pub fn save_bookmark(&mut self, bookmark: Bookmark) {
        let name = bookmark.name();
        match self.bookmarks.iter_mut().find(|b| b.name() == name) {
            Some(old) => *old = bookmark,
            None => self.bookmarks.push(bookmark),
        }
    }

    /// The server of the first bookmark with the name in `auto_connect`.
    pub fn auto_connect(&self, encoding: &'static Encoding) -> Option<Target> {
        let name = self.auto_connect.as_deref()?;
        self.bookmarks
            .iter()
            .find(|bookmark| *bookmark.name() == *name)?
            .target(encoding)
    }
}

impl Config {
    /// The options of the options form (Alt-O).
    pub fn user_options(&self) -> UserChangeOptions {
        [
            (UserChangeOptions::REFUSE_PM, self.refuse_pm),
            (UserChangeOptions::REFUSE_CHAT, self.refuse_chat),
            (
                UserChangeOptions::AUTOMATIC_RESPONSE,
                self.automatic_response,
            ),
        ]
        .into_iter()
        .filter_map(|(flag, on)| on.then_some(flag))
        .collect()
    }

    /// Set the options of the options form (Alt-O).
    pub const fn set_user_options(&mut self, options: UserChangeOptions) {
        self.refuse_pm = options.contains(UserChangeOptions::REFUSE_PM);
        self.refuse_chat = options.contains(UserChangeOptions::REFUSE_CHAT);
        self.automatic_response = options.contains(UserChangeOptions::AUTOMATIC_RESPONSE);
    }
}

/// A [`Config`] and the file that it comes from.
#[derive(Debug)]
pub struct ConfigFile {
    pub config: Config,
    /// `None` when neither `XDG_CONFIG_HOME` nor `HOME` is set.
    path: Option<PathBuf>,
}

impl ConfigFile {
    /// Read the settings file. A file that does not exist gives the defaults.
    pub fn load() -> anyhow::Result<Self> {
        let path = path(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        );
        Ok(Self {
            config: match &path {
                Some(path) => read(path)?,
                None => Config::default(),
            },
            path,
        })
    }

    /// Write the settings file, and make its folder if it does not exist.
    pub fn save(&self) -> anyhow::Result<()> {
        match &self.path {
            Some(path) => write(path, &self.config),
            None => Ok(()),
        }
    }
}

/// The path of the settings file: `$XDG_CONFIG_HOME/hlc/config.toml`, or
/// `$HOME/.config/hlc/config.toml` when `XDG_CONFIG_HOME` is not an absolute
/// path.
fn path(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = xdg_config_home
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home.map(|home| Path::new(&home).join(".config")))?;
    Some(base.join("hlc").join("config.toml"))
}

/// Read the settings at `path`, or the defaults if `path` does not exist.
fn read(path: &Path) -> anyhow::Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).with_context(|| path.display().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| path.display().to_string()),
    }
}

/// Write `config` to `path`. Each setting that stays the same keeps its
/// text and its comments. On Unix, only the owner can read the file.
fn write(path: &Path, config: &Config) -> anyhow::Result<()> {
    let new = toml::to_string(config)?;
    let text = match std::fs::read_to_string(path) {
        Ok(old) => edit::update(&old, &new).unwrap_or(new),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => new,
        Err(e) => return Err(e).with_context(|| path.display().to_string()),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
    }
    write_private(path, text.as_bytes()).with_context(|| path.display().to_string())
}

/// Write `bytes` to `path` with mode 0600.
#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)
}

/// Write `bytes` to `path`.
#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_takes_an_absolute_xdg_config_home() {
        assert_eq!(
            path(Some("/x".into()), Some("/home/a".into())),
            Some(PathBuf::from("/x/hlc/config.toml"))
        );
    }

    #[test]
    fn path_falls_back_to_home_for_a_relative_or_missing_xdg_config_home() {
        let expected = Some(PathBuf::from("/home/a/.config/hlc/config.toml"));
        assert_eq!(path(Some("x".into()), Some("/home/a".into())), expected);
        assert_eq!(path(None, Some("/home/a".into())), expected);
        assert_eq!(path(None, None), None);
    }

    #[test]
    fn read_gives_the_defaults_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = read(&dir.path().join("config.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn read_fills_each_missing_setting_with_its_default() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(
            &file,
            "name = \"Alice\"\n\n[bell]\nchat = true\nroom = false\n",
        )
        .unwrap();
        let config = read(&file).unwrap();
        assert_eq!(config.name.as_deref(), Some("Alice"));
        assert_eq!(config.bell, (Bell::DEFAULT - Bell::ROOM) | Bell::CHAT);
        assert!(config.queue_transfers);
    }

    #[test]
    fn write_makes_the_folder_and_read_gives_the_same_settings() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hlc").join("config.toml");
        let config = Config {
            name: Some("Alice".into()),
            icon: 128,
            queue_transfers: false,
            automatic_response: true,
            automatic_response_text: "away".into(),
            refuse_pm: true,
            refuse_chat: false,
            bell: Bell::empty(),
            ..Config::default()
        };
        write(&file, &config).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.contains(
                "automatic_response = true\nautomatic_response_text = \"away\"\nrefuse_pm = true\n"
            ),
            "{text}"
        );
        assert_eq!(read(&file).unwrap(), config);
    }

    #[test]
    fn write_leaves_out_a_blank_automatic_response_text() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        write(&file, &Config::default()).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("automatic_response = false\n"), "{text}");
        assert!(!text.contains("automatic_response_text"), "{text}");
    }

    #[test]
    fn user_options_gives_back_the_options_that_set_user_options_takes() {
        let mut config = Config::default();
        let options = UserChangeOptions::REFUSE_CHAT | UserChangeOptions::AUTOMATIC_RESPONSE;
        config.set_user_options(options);
        assert_eq!(config.user_options(), options);
        config.set_user_options(UserChangeOptions::all());
        assert_eq!(config.user_options(), UserChangeOptions::all());
    }

    #[test]
    fn write_keeps_the_comments_of_the_user() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "# my name\nname = \"Alice\"\nicon = 0\n").unwrap();
        let mut config = read(&file).unwrap();
        config.icon = 128;
        write(&file, &config).unwrap();

        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.starts_with("# my name\nname = \"Alice\"\nicon = 128\n"),
            "{text}"
        );
        assert_eq!(read(&file).unwrap(), config);
    }

    #[test]
    fn write_and_read_keep_each_bookmark() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let config = Config {
            bookmarks: vec![
                Bookmark {
                    name: Some("Home".into()),
                    address: "example.com:5600".into(),
                    login: "alice".into(),
                    password: "secret".into(),
                },
                Bookmark {
                    name: Some("Guest".into()),
                    address: "example.org".into(),
                    login: "".into(),
                    password: "".into(),
                },
            ],
            auto_connect: Some("Home".into()),
            ..Config::default()
        };
        write(&file, &config).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.contains("[[bookmarks]]\nname = \"Guest\"\naddress = \"example.org\"\n"),
            "{text}"
        );
        assert_eq!(read(&file).unwrap(), config);
    }

    #[test]
    fn write_leaves_out_the_default_name_of_a_bookmark() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let bookmark = Bookmark {
            name: None,
            address: "example.com".into(),
            login: "alice".into(),
            password: "".into(),
        };
        let config = Config {
            bookmarks: vec![bookmark.clone()],
            ..Config::default()
        };
        write(&file, &config).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("name ="), "{text}");
        assert_eq!(read(&file).unwrap(), config);
        assert_eq!(&*bookmark.name(), "alice@example.com");
    }

    #[test]
    fn read_gives_the_default_name_to_a_bookmark_with_an_empty_name() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(
            &file,
            "[[bookmarks]]\nname = \"\"\naddress = \"example.com\"\nlogin = \"bob\"\n",
        )
        .unwrap();
        assert_eq!(
            &*read(&file).unwrap().bookmarks[0].name(),
            "bob@example.com"
        );
    }

    #[test]
    fn read_refuses_an_unknown_bookmark_field() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(
            &file,
            "[[bookmarks]]\nname = \"a\"\naddress = \"b\"\nport = 1\n",
        )
        .unwrap();
        assert!(read(&file).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn write_lets_only_the_owner_read_the_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        write(&file, &Config::default()).unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn save_bookmark_replaces_the_bookmark_of_the_same_name() {
        let mut config = Config::default();
        config.save_bookmark(bookmark("a", "one"));
        config.save_bookmark(bookmark("b", "two"));
        config.save_bookmark(bookmark("a", "three"));
        assert_eq!(
            config.bookmarks,
            [bookmark("a", "three"), bookmark("b", "two")]
        );
    }

    fn bookmark(name: &str, address: &str) -> Bookmark {
        Bookmark {
            name: Some(name.into()),
            address: address.into(),
            login: "alice".into(),
            password: "secret".into(),
        }
    }

    #[test]
    fn auto_connect_takes_the_bookmark_that_it_names() {
        let encoding = hotline_proto::text_encoding::DEFAULT;
        let mut config = Config {
            bookmarks: vec![
                bookmark("a", "one"),
                bookmark("b", "two:6000"),
                bookmark("b", "three"),
            ],
            ..Config::default()
        };
        assert!(config.auto_connect(encoding).is_none());
        config.auto_connect = Some("c".into());
        assert!(config.auto_connect(encoding).is_none());
        config.auto_connect = Some("b".into());
        let target = config.auto_connect(encoding).unwrap();
        assert_eq!(
            (target.address(), target.login, target.password),
            ("two:6000".into(), "alice".into(), "secret".into())
        );
    }

    #[test]
    fn auto_connect_gives_nothing_for_a_bad_address() {
        let config = Config {
            bookmarks: vec![bookmark("a", "one:x"), bookmark("b", "two")],
            auto_connect: Some("a".into()),
            ..Config::default()
        };
        assert!(
            config
                .auto_connect(hotline_proto::text_encoding::DEFAULT)
                .is_none()
        );
    }
}
