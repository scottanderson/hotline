//! Server banner, pushed to a client at login (`myTran_ServerBanner`, 122)
//! and, for an image banner, fetched over `HTXF` by `myTran_DownloadBanner`
//! (212).
//!
//! [`BannerProvider`] is the storage interface; `hotline-server` only
//! defines it and provides [`NoBanner`] (no banner configured) and
//! [`UrlBannerProvider`] (a fixed URL, nothing to fetch over `HTXF`) as
//! defaults. The embedding application supplies the real backend to
//! [`crate::Server::new`], the same way it supplies a [`crate::FileProvider`].

mod kind;
mod none;
mod provider;
mod url;

pub use kind::BannerKind;
pub use none::NoBanner;
pub use provider::BannerProvider;
pub use url::UrlBannerProvider;
