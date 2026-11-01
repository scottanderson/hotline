use std::sync::Arc;

use crate::account::{AccountProvider, AccountStores, InMemoryAccountProvider, NoAccountStores};
use crate::activity::{ActivityLog, NoActivityLog};
use crate::ban::{BanProvider, InMemoryBanProvider};
use crate::banner::{BannerProvider, NoBanner};
use crate::chat_room::{ChatRoomProvider, InMemoryChatRoomProvider};
use crate::console::{ChatCommandDispatcher, DefaultChatCommands};
use crate::file::{FileProvider, InMemoryFileProvider};
use crate::flood::{FloodPolicy, NoFloodPolicy};
use crate::news::{FlatNewsProvider, InMemoryNewsProvider, ThreadedNewsProvider};
use crate::policy::{OpenlinePolicy, ServerPolicy};
use crate::user_list::{InMemoryUserListProvider, UserListProvider};

/// The pluggable backends [`crate::Server`]/[`crate::SharedState`] run on,
/// each supplied by the embedding application.
pub struct Providers {
    /// See [`AccountProvider`].
    pub accounts: Arc<dyn AccountProvider>,
    /// See [`FileProvider`].
    pub files: Arc<dyn FileProvider>,
    /// See [`FlatNewsProvider`].
    pub flat_news: Arc<dyn FlatNewsProvider>,
    /// See [`ThreadedNewsProvider`].
    pub threaded_news: Arc<dyn ThreadedNewsProvider>,
    /// See [`AccountStores`].
    pub account_stores: Arc<dyn AccountStores>,
    /// See [`BanProvider`].
    pub bans: Arc<dyn BanProvider>,
    /// See [`FloodPolicy`].
    pub flood: Arc<dyn FloodPolicy>,
    /// See [`BannerProvider`].
    pub banner: Arc<dyn BannerProvider>,
    /// See [`ChatCommandDispatcher`].
    pub commands: Arc<dyn ChatCommandDispatcher>,
    /// See [`ActivityLog`].
    pub activity: Arc<dyn ActivityLog>,
    /// See [`ServerPolicy`].
    pub policy: Arc<dyn ServerPolicy>,
    /// See [`UserListProvider`].
    pub user_list: Arc<dyn UserListProvider>,
    /// See [`ChatRoomProvider`].
    pub chat_rooms: Arc<dyn ChatRoomProvider>,
}

/// In-memory storage, no account stores, no flood policy, no banner, no
/// activity log, and openline's behavior.
/// Classic and threaded news share one provider.
impl Default for Providers {
    fn default() -> Self {
        let news = Arc::new(InMemoryNewsProvider::default());
        Self {
            accounts: Arc::new(InMemoryAccountProvider::default()),
            files: Arc::new(InMemoryFileProvider::default()),
            flat_news: news.clone(),
            threaded_news: news,
            account_stores: Arc::new(NoAccountStores),
            bans: Arc::new(InMemoryBanProvider::default()),
            flood: Arc::new(NoFloodPolicy),
            banner: Arc::new(NoBanner),
            commands: Arc::new(DefaultChatCommands),
            activity: Arc::new(NoActivityLog),
            policy: Arc::new(OpenlinePolicy),
            user_list: Arc::new(InMemoryUserListProvider::default()),
            chat_rooms: Arc::new(InMemoryChatRoomProvider::default()),
        }
    }
}
