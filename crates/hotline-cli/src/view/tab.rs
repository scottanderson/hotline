/// The screen shown in the main area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Chat,
    News,
    Files,
    /// Threaded news bundles, categories, and articles.
    Threaded,
    /// User accounts, for a user who may administer them.
    Admin,
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

    /// The label of [`Tab::Admin`].
    pub const ADMIN_LABEL: &'static str = "Admin";

    /// The ID of the private chat room, for [`Tab::Room`].
    pub const fn chat_id(self) -> Option<u32> {
        match self {
            Self::Room(chat_id) => Some(chat_id),
            _ => None,
        }
    }
}
