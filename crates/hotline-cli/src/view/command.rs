use super::Tab;

/// One entry in the command palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Open this tab.
    OpenTab(Tab),
    Broadcast,
    Options,
    CancelDownload,
    /// Invite a user to the room `chat_id`.
    Invite(u32),
    /// Set the subject of the room `chat_id`.
    Subject(u32),
    /// Leave the room `chat_id`.
    Leave(u32),
    Quit,
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
            Self::CancelDownload => "Cancel a download".into(),
            Self::Invite(_) => "Invite to this room".into(),
            Self::Subject(_) => "Set the room subject".into(),
            Self::Leave(_) => "Leave this room".into(),
            Self::Quit => "Quit".into(),
        }
    }

    /// The key that runs the command without the palette.
    pub const fn key(self) -> &'static str {
        match self {
            Self::OpenTab(_) => "",
            Self::Broadcast => "^G",
            Self::Options => "^O",
            Self::CancelDownload => "^K",
            Self::Invite(_) => "^A",
            Self::Subject(_) => "^T",
            Self::Leave(_) => "^W",
            Self::Quit => "^C",
        }
    }
}

/// The command palette: the commands that match `input`.
#[derive(Debug, PartialEq, Eq)]
pub struct Palette {
    pub input: String,
    /// Each command and its label, in palette order.
    commands: Vec<(Command, String)>,
    pub selected: usize,
}

impl Palette {
    pub fn new(commands: impl IntoIterator<Item = Command>, tabs: &[(Tab, String)]) -> Self {
        Self {
            input: String::new(),
            commands: commands
                .into_iter()
                .map(|command| (command, command.label(tabs)))
                .collect(),
            selected: 0,
        }
    }

    /// The commands whose label holds `input`, ignoring case.
    pub fn matches(&self) -> impl Iterator<Item = &(Command, String)> {
        let input = self.input.to_lowercase();
        self.commands
            .iter()
            .filter(move |(_, label)| label.to_lowercase().contains(&input))
    }

    /// The selected command, if any matches.
    pub fn chosen(&self) -> Option<Command> {
        self.matches()
            .nth(self.selected)
            .map(|&(command, _)| command)
    }

    pub const fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_next(&mut self) {
        let last = self.matches().count().saturating_sub(1);
        self.selected = (self.selected + 1).min(last);
    }

    /// Change the filter text with `edit`, and select the first match.
    pub fn edit(&mut self, edit: impl FnOnce(&mut String)) {
        edit(&mut self.input);
        self.selected = 0;
    }
}
