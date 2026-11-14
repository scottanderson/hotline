use hotline_proto::Access;

use super::{MenuItem, Tab, TextBox};

/// One entry in the command palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Open this tab.
    OpenTab(Tab),
    Broadcast,
    Options,
    /// Go to the Tasks tab, or from it back to the tab before it.
    Tasks,
    /// Invite a user to the room `chat_id`.
    Invite(u32),
    /// Set the subject of the room `chat_id`.
    Subject(u32),
    /// Leave the room `chat_id`.
    Leave(u32),
    /// Log in again to the server of the last connection.
    Reconnect,
    /// End the connection, and stay open.
    Disconnect,
    /// Ask for a server, then connect to it.
    ConnectTo,
    Quit,
    /// Run `item` of the user menu on the user `user_id`.
    User(u16, MenuItem),
}

impl Command {
    /// The text the palette shows and matches.
    pub fn label(self, tabs: &[(Tab, String)]) -> String {
        match self {
            Self::OpenTab(tab) => {
                let label = tabs.iter().find(|&&(t, _)| t == tab).map(|(_, l)| l);
                format!("Go to {}", label.map_or("", String::as_str))
            }
            Self::Broadcast => "Broadcast".into(),
            Self::Options => "Options".into(),
            Self::Tasks => "Show or hide the tasks".into(),
            Self::Invite(_) => "Invite to this room".into(),
            Self::Subject(_) => "Set the room subject".into(),
            Self::Leave(_) => "Leave this room".into(),
            Self::Reconnect => "Reconnect".into(),
            Self::Disconnect => "Disconnect".into(),
            Self::ConnectTo => "Connect to".into(),
            Self::Quit => "Quit".into(),
            Self::User(_, item) => item.label().into(),
        }
    }

    /// The key that runs the command without the palette. A tab of the
    /// tab bar `bar_tabs` has its function key.
    pub fn key(self, bar_tabs: &[(Tab, String)]) -> &'static str {
        match self {
            Self::OpenTab(tab) => bar_tabs
                .iter()
                .position(|&(t, _)| t == tab)
                .and_then(|i| TAB_KEYS.get(i))
                .copied()
                .unwrap_or_default(),
            Self::Broadcast => "Alt-A",
            Self::Options => "Alt-O",
            Self::Tasks => "Alt-T",
            Self::Invite(_) => "Alt-I",
            Self::Subject(_) => "Alt-S",
            Self::Leave(_) => "Alt-W",
            Self::Quit => "^C",
            Self::User(_, MenuItem::SendMessage) => "Enter",
            Self::User(_, MenuItem::GetInfo) => "i",
            Self::User(_, MenuItem::Disconnect) => "k",
            Self::Reconnect
            | Self::Disconnect
            | Self::ConnectTo
            | Self::User(_, MenuItem::PrivateChat) => "",
        }
    }

    /// The privileges that turn the command on. Any one of them is enough.
    pub const fn enabled_by(self) -> Access {
        match self {
            Self::Broadcast => Access::BROADCAST,
            Self::User(_, item) => item.enabled_by(),
            _ => Access::empty(),
        }
    }
}

/// The keys of the tabs of the tab bar, in order.
const TAB_KEYS: [&str; 9] = ["F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9"];

/// One row of the palette.
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub command: Command,
    /// The text the palette shows and matches.
    pub label: String,
    /// The key that runs the command, or `""`.
    pub key: &'static str,
}

/// The command palette, or the action menu: the commands that match `input`.
#[derive(Debug, PartialEq, Eq)]
pub struct Palette {
    /// The title shown while `input` is empty.
    pub title: &'static str,
    pub input: TextBox,
    /// The rows, in palette order.
    entries: Vec<Entry>,
    pub selected: usize,
}

impl Palette {
    /// A palette of `commands`. `tabs` names each tab, and `bar_tabs` are
    /// the tabs of the tab bar.
    pub fn new(
        title: &'static str,
        commands: impl IntoIterator<Item = Command>,
        tabs: &[(Tab, String)],
        bar_tabs: &[(Tab, String)],
    ) -> Self {
        Self {
            title,
            input: TextBox::new(),
            entries: commands
                .into_iter()
                .map(|command| Entry {
                    command,
                    label: command.label(tabs),
                    key: command.key(bar_tabs),
                })
                .collect(),
            selected: 0,
        }
    }

    /// The rows whose label holds `input`, ignoring case.
    pub fn matches(&self) -> impl Iterator<Item = &Entry> {
        let input = self.input.as_str().to_lowercase();
        self.entries
            .iter()
            .filter(move |entry| entry.label.to_lowercase().contains(&input))
    }

    /// The selected command, if any matches.
    pub fn chosen(&self) -> Option<Command> {
        self.matches().nth(self.selected).map(|entry| entry.command)
    }

    pub const fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_next(&mut self) {
        let last = self.matches().count().saturating_sub(1);
        self.selected = self.selected.saturating_add(1).min(last);
    }

    /// Change the filter text with `edit`. When `edit` returns `true`,
    /// select the first match.
    pub fn edit(&mut self, edit: impl FnOnce(&mut TextBox) -> bool) {
        if edit(&mut self.input) {
            self.selected = 0;
        }
    }
}
