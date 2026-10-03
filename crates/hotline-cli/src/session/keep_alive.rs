//! The request that keeps an idle connection open.

use std::time::Duration;

use hotline_client::HotlineClient;

use super::ClientSession;
use crate::line::LineKind;

/// The time without traffic before [`ClientSession::keep_alive`] runs.
pub const KEEP_ALIVE: Duration = Duration::from_secs(180);

/// The lowest server version that takes [`myTran_KeepConnectionAlive`] (500).
///
/// [`myTran_KeepConnectionAlive`]: hotline_proto::Opcode::KeepConnectionAlive
const KEEP_ALIVE_VERSION: u16 = 185;

impl ClientSession {
    /// Send [`myTran_KeepConnectionAlive`] (500), or on an older server
    /// fetch the user list again, [`myTran_GetUserNameList`] (300).
    ///
    /// [`myTran_KeepConnectionAlive`]: hotline_proto::Opcode::KeepConnectionAlive
    /// [`myTran_GetUserNameList`]: hotline_proto::Opcode::GetUserNameList
    pub async fn keep_alive(&mut self, client: &HotlineClient) {
        if self.server_at_least(KEEP_ALIVE_VERSION) {
            if let Err(e) = client.keep_connection_alive().await {
                self.push(
                    format!("failed to keep the connection: {e}"),
                    LineKind::Error,
                );
            }
            return;
        }
        match client.get_user_name_list().await {
            Ok(users) => self.users = users,
            Err(e) => self.push(format!("failed to fetch user list: {e}"), LineKind::Error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support;

    #[tokio::test]
    async fn keep_alive_on_an_old_server_fetches_the_user_list() {
        let addr = test_support::start_server(hotline_server::Providers::default()).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me".to_string(), 0, client.encoding());
        let before = session.lines.len();

        session.server_version = Some(190);
        session.keep_alive(&client).await;
        assert!(session.users.is_empty());

        session.server_version = Some(151);
        session.keep_alive(&client).await;
        assert_eq!(session.users.len(), 1);
        assert_eq!(session.lines.len(), before);
    }
}
