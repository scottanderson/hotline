//! The settings file of `hlc`, `$XDG_CONFIG_HOME/hlc/config.toml`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::Context;
use hotline_proto::types::UserChangeOptions;
use serde::{Deserialize, Serialize};

use crate::bell::Bell;

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
        }
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
/// text and its comments.
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
    std::fs::write(path, text).with_context(|| path.display().to_string())
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
}
