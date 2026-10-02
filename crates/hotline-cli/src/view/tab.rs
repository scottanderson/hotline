/// The screen shown in the main area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Chat,
    News,
    Files,
    /// Threaded news bundles, categories, and articles.
    Threaded,
    /// The private chat room with this ID.
    Room(u32),
}

impl Tab {
    /// The tabs before the rooms, with their labels.
    pub const FIXED: [(Self, &'static str); 4] = [
        (Self::Chat, "Chat"),
        (Self::News, "News"),
        (Self::Files, "Files"),
        (Self::Threaded, "Threaded News"),
    ];
}
