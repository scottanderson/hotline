/// The screen shown in the main area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Chat,
    News,
    Files,
}

impl Tab {
    pub const ALL: [Self; 3] = [Self::Chat, Self::News, Self::Files];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::News => "News",
            Self::Files => "Files",
        }
    }
}
