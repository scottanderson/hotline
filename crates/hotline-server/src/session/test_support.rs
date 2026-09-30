//! Shared fixtures for the `session` module's tests.

use std::time::Instant;

use tokio::sync::mpsc;

use hotline_proto::consts::Access;
use hotline_proto::opcode::{Opcode, TransactionType};
use hotline_proto::text_encoding::EncodingHandle;

use crate::account::AccessExtra;
use crate::server::ServerConfig;
use crate::state::{AccessUpdate, RecipientMessage, SharedState, UserEntry};

use super::connection::{Session, SessionState};

use crate::providers::Providers;

pub(super) fn test_session(name: &str, access: Access) -> Session {
    Session {
        user_id: 1,
        name: name.to_string(),
        login: String::new(),
        account_name: String::new(),
        access,
        access_extra: AccessExtra::default(),
        client_version: None,
        state: SessionState::Joined,
        is_away: false,
        refuses_pm: false,
        refuses_chat: false,
        automatic_response: None,
        remote_addr: "127.0.0.1:0".parse().unwrap(),
        connected_at: Instant::now(),
        encoding: EncodingHandle::default(),
    }
}

/// Shared state with openline's flood limits.
pub(super) fn state_with_flood() -> SharedState {
    SharedState::new(Providers {
        accounts: std::sync::Arc::new(crate::account::InMemoryAccountProvider::default()),
        files: std::sync::Arc::new(crate::file::InMemoryFileProvider::default()),
        flat_news: std::sync::Arc::new(crate::news::InMemoryNewsProvider::default()),
        threaded_news: std::sync::Arc::new(crate::news::InMemoryNewsProvider::default()),
        bans: std::sync::Arc::new(crate::ban::InMemoryBanProvider::default()),
        flood: std::sync::Arc::new(crate::flood::InMemoryFloodPolicy::default()),
        banner: std::sync::Arc::new(crate::banner::NoBanner),
        commands: std::sync::Arc::new(crate::console::DefaultChatCommands),
    })
}

/// Config with console commands switched on under `/`.
pub(super) fn console_config() -> ServerConfig {
    ServerConfig {
        console_prefix: Some('/'),
        ..ServerConfig::default()
    }
}

/// A minimal connected [`UserEntry`], for tests exercising
/// private-message/chat-invite refusal against another user.
pub(super) fn test_user(user_id: u16, name: &str) -> UserEntry {
    UserEntry {
        user_id,
        login: String::new(),
        account_name: String::new(),
        name: name.to_string(),
        icon: 0,
        client_version: None,
        access: Access::all(),
        access_extra: AccessExtra::default(),
        remote_addr: "127.0.0.1:0".parse().unwrap(),
        connected_at: Instant::now(),
        last_active: Instant::now(),
        refuses_pm: false,
        refuses_chat: false,
        automatic_response: None,
    }
}

/// Drains `rx` until an `AccessUpdate` arrives, skipping the
/// `UserChange` push a prior `apply_access_update` already queued.
pub(super) fn next_access_update(rx: &mut mpsc::Receiver<RecipientMessage>) -> AccessUpdate {
    loop {
        match rx.try_recv().expect("expected an access update") {
            RecipientMessage::AccessUpdate(update) => return update,
            RecipientMessage::Push(tx) => {
                assert_eq!(tx.r#type(), TransactionType::Push(Opcode::NotifyChangeUser));
            }
            RecipientMessage::Console(_) => panic!("unexpected console update"),
            RecipientMessage::Disconnect => panic!("unexpected disconnect"),
        }
    }
}

pub(super) fn state_with_banner(
    banner: std::sync::Arc<dyn crate::banner::BannerProvider>,
) -> SharedState {
    SharedState::new(Providers {
        accounts: std::sync::Arc::new(crate::account::InMemoryAccountProvider::default()),
        files: std::sync::Arc::new(crate::file::InMemoryFileProvider::default()),
        flat_news: std::sync::Arc::new(crate::news::InMemoryNewsProvider::default()),
        threaded_news: std::sync::Arc::new(crate::news::InMemoryNewsProvider::default()),
        bans: std::sync::Arc::new(crate::ban::InMemoryBanProvider::default()),
        flood: std::sync::Arc::new(crate::flood::NoFloodPolicy),
        banner,
        commands: std::sync::Arc::new(crate::console::DefaultChatCommands),
    })
}

/// A fixed-bytes [`crate::banner::BannerProvider`], for exercising the
/// [`myTran_DownloadBanner`] (212) and [`myTran_ServerBanner`] (122) login push
/// paths.
///
/// [`myTran_DownloadBanner`]: hotline_proto::Opcode::DownloadBanner
/// [`myTran_ServerBanner`]: hotline_proto::Opcode::ServerBanner
pub(super) struct TestImageBanner {
    pub(super) format: [u8; 4],
    pub(super) bytes: Vec<u8>,
}

#[async_trait::async_trait]
impl crate::banner::BannerProvider for TestImageBanner {
    async fn kind(&self) -> Option<crate::banner::BannerKind> {
        Some(crate::banner::BannerKind::Image {
            format: self.format,
        })
    }

    async fn open_read(&self) -> std::io::Result<crate::file::BoxedReader> {
        Ok(Box::new(std::io::Cursor::new(self.bytes.clone())))
    }

    async fn size(&self) -> std::io::Result<u64> {
        Ok(self.bytes.len() as u64)
    }
}
