//! Shared fixtures for the `session` module's tests.

use std::sync::Arc;
use std::time::Instant;

use hotline_proto::consts::Access;
use hotline_proto::opcode::{Opcode, TransactionType};
use hotline_proto::text_encoding::{self, EncodingHandle};
use hotline_proto::{Field, Received, Transaction};

use crate::account::AccessExtra;
use crate::activity::{Activity, ActivityEntry, ActivityLog};
use crate::server::ServerConfig;
use crate::state::{AccessUpdate, Inbox, RecipientMessage, SharedState, UserEntry};

use super::connection::{Session, SessionState};

use crate::providers::Providers;

pub(super) fn test_session(name: &str, access: Access) -> Session {
    Session {
        user_id: 1,
        name: name.into(),
        real_name: name.into(),
        login: Box::default(),
        account_name: Box::default(),
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

/// A request as the server reads it in the default encoding.
pub(super) fn request(opcode: Opcode, trans: u32, fields: Vec<Field>) -> Transaction<Received> {
    Transaction::request(opcode, trans, fields).received(text_encoding::DEFAULT)
}

/// Shared state with openline's flood limits.
pub(super) fn state_with_flood() -> Arc<SharedState> {
    SharedState::new(Providers {
        flood: Arc::new(crate::flood::InMemoryFloodPolicy::default()),
        ..Providers::default()
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
        login: Box::default(),
        account_name: Box::default(),
        name: name.into(),
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

/// Drains `rx` until an `AccessUpdate` arrives, skipping the pushes a
/// prior `apply_access_update` already queued.
pub(super) fn next_access_update(rx: &mut Inbox) -> AccessUpdate {
    loop {
        match rx.try_recv().expect("expected an access update") {
            RecipientMessage::AccessUpdate(update) => return update,
            RecipientMessage::Push(tx) | RecipientMessage::Broadcast(tx) => {
                assert!(matches!(
                    tx.r#type(),
                    TransactionType::Push(Opcode::NotifyChangeUser | Opcode::UserAccess)
                ));
            }
            RecipientMessage::Console(_) => panic!("unexpected console update"),
            RecipientMessage::Kick(_) => panic!("unexpected kick"),
        }
    }
}

/// State with `policy`'s behavior choices.
pub(super) fn state_with_policy(policy: impl crate::ServerPolicy + 'static) -> Arc<SharedState> {
    SharedState::new(Providers {
        policy: Arc::new(policy),
        ..Providers::default()
    })
}

pub(super) fn state_with_banner(
    banner: Arc<dyn crate::banner::BannerProvider>,
) -> Arc<SharedState> {
    SharedState::new(Providers {
        banner,
        ..Providers::default()
    })
}

/// State over the given account storage.
pub(super) fn state_with_accounts(
    accounts: Arc<dyn crate::account::AccountProvider>,
) -> Arc<SharedState> {
    SharedState::new(Providers {
        accounts,
        ..Providers::default()
    })
}

/// An [`ActivityLog`] that keeps every entry.
#[derive(Default)]
pub(super) struct RecordingActivityLog(std::sync::Mutex<Vec<ActivityEntry>>);

impl RecordingActivityLog {
    /// The recorded activities, oldest first.
    pub(super) fn activities(&self) -> Vec<Activity> {
        let entries = self.0.lock().unwrap();
        entries.iter().map(|entry| entry.activity.clone()).collect()
    }
}

#[async_trait::async_trait]
impl ActivityLog for RecordingActivityLog {
    async fn record(&self, entry: ActivityEntry) {
        self.0.lock().unwrap().push(entry);
    }
}

/// State over `files` whose activity goes to the returned log.
pub(super) fn state_recording_activity(
    files: crate::file::InMemoryFileProvider,
) -> (Arc<SharedState>, Arc<RecordingActivityLog>) {
    let log = Arc::new(RecordingActivityLog::default());
    let state = SharedState::new(Providers {
        files: Arc::new(files),
        activity: log.clone(),
        ..Providers::default()
    });
    (state, log)
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
