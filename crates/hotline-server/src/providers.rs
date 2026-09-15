use std::sync::Arc;

use crate::account::AccountProvider;
use crate::ban::BanProvider;
use crate::banner::BannerProvider;
use crate::console::ChatCommandDispatcher;
use crate::file::FileProvider;
use crate::flood::FloodPolicy;
use crate::news::{FlatNewsProvider, ThreadedNewsProvider};

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
    /// See [`BanProvider`].
    pub bans: Arc<dyn BanProvider>,
    /// See [`FloodPolicy`].
    pub flood: Arc<dyn FloodPolicy>,
    /// See [`BannerProvider`].
    pub banner: Arc<dyn BannerProvider>,
    /// See [`ChatCommandDispatcher`].
    pub commands: Arc<dyn ChatCommandDispatcher>,
}
