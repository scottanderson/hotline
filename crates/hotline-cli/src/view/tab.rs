use hotline_proto::Access;

/// The screen shown in the main area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Chat,
    News,
    Files,
    /// Threaded news bundles, categories, and articles.
    Threaded,
    /// Each transfer, and each request that waits for its reply.
    Tasks,
    /// User accounts, for a user who may administer them.
    Admin,
    /// The private chat room with this ID.
    Room(u32),
}

impl Tab {
    /// The first tabs of the tab bar, with their labels.
    pub const FIXED: [(Self, &'static str); 4] = [
        (Self::Chat, "Chat"),
        (Self::News, "News"),
        (Self::Files, "Files"),
        (Self::Tasks, "Tasks"),
    ];

    /// The label of [`Tab::Admin`].
    pub const ADMIN_LABEL: &'static str = "Admin";

    /// The label of [`Tab::Threaded`] in the row under the News tab.
    pub const THREADED_LABEL: &'static str = "Threaded News";

    /// The tab in the tab bar that shows `self`: [`Tab::Chat`] for a room,
    /// and [`Tab::News`] for the threaded news.
    pub const fn top(self) -> Self {
        match self {
            Self::Room(_) => Self::Chat,
            Self::Threaded => Self::News,
            tab => tab,
        }
    }

    /// The label of a tab of the tab bar in the row under it.
    pub const fn row_label(self) -> Option<&'static str> {
        match self {
            Self::Chat => Some("Public"),
            Self::News => Some("Message Board"),
            _ => None,
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
