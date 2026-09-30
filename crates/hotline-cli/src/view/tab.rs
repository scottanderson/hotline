/// The screen shown in the main area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Chat,
    News,
}

impl Tab {
    pub const ALL: [Self; 2] = [Self::Chat, Self::News];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::News => "News",
        }
    }
}
