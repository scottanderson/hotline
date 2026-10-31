use hotline_proto::Access;

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

    /// The label of [`Tab::Chat`] in the room row.
    pub const PUBLIC_LABEL: &'static str = "Public";

    /// The tab in the tab bar that shows `self`: [`Tab::Chat`] for a room.
    pub const fn top(self) -> Self {
        match self {
            Self::Room(_) => Self::Chat,
            tab => tab,
        }
    }

    /// The privileges that turn the tab on. Any one of them is enough.
    pub const fn enabled_by(self) -> Access {
        match self {
            Self::Chat => Access::READ_CHAT.union(Access::SEND_CHAT),
            Self::News | Self::Threaded => Access::NEWS_READ_ART,
            Self::Admin => Access::OPEN_USER.union(Access::CREATE_USER),
            _ => Access::empty(),
        }
    }

    /// The ID of the private chat room, for [`Tab::Room`].
    pub const fn chat_id(self) -> Option<u32> {
        match self {
            Self::Room(chat_id) => Some(chat_id),
            _ => None,
        }
    }
}
