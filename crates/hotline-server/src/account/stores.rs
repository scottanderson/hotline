use std::sync::Arc;

use async_trait::async_trait;

use crate::file::FileProvider;
use crate::news::ThreadedNewsProvider;

/// The file and threaded news stores that belong to one account. An
/// account without its own store uses the shared one in
/// [`crate::Providers`].
#[async_trait]
pub trait AccountStores: Send + Sync {
    /// `login`'s own files, or `None` for the shared files. `Err` if
    /// `login` has its own files and they do not open.
    async fn files(&self, login: &str) -> Result<Option<Arc<dyn FileProvider>>, &'static str>;

    /// `login`'s own threaded news, or `None` for the shared threaded
    /// news. `Err` if `login` has its own threaded news and it does not
    /// open.
    async fn threaded_news(
        &self,
        login: &str,
    ) -> Result<Option<Arc<dyn ThreadedNewsProvider>>, &'static str>;
}

/// [`AccountStores`] where no account has its own store.
#[derive(Debug, Default)]
pub struct NoAccountStores;

#[async_trait]
impl AccountStores for NoAccountStores {
    async fn files(&self, _login: &str) -> Result<Option<Arc<dyn FileProvider>>, &'static str> {
        Ok(None)
    }

    async fn threaded_news(
        &self,
        _login: &str,
    ) -> Result<Option<Arc<dyn ThreadedNewsProvider>>, &'static str> {
        Ok(None)
    }
}
