//! The request that keeps an idle connection open.

use std::time::Duration;

use hotline_client::HotlineClient;

use super::{ClientSession, Request};

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
    pub fn keep_alive(&self, client: &HotlineClient) -> Request<()> {
        let client = client.clone();
        if self.server_at_least(KEEP_ALIVE_VERSION) {
            return Request::report(
                async move { client.keep_connection_alive().await },
                "failed to keep the connection",
                |_, ()| {},
            );
        }
        Request::report(
            async move { client.get_user_name_list().await },
            "failed to fetch user list",
            |session: &mut Self, users| session.users = users,
        )
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
        let mut session = ClientSession::new("me", 0, client.encoding());
        let before = session.lines.len();

        session.server_version = Some(190);
        session.keep_alive(&client).run(&mut session).await;
        assert!(session.users.is_empty());

        session.server_version = Some(151);
        session.keep_alive(&client).run(&mut session).await;
        assert_eq!(session.users.len(), 1);
        assert_eq!(session.lines.len(), before);
    }
}
