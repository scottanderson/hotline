use hotline_proto::consts::Access;

use crate::account::AccessExtra;

/// The session that runs a console command.
#[derive(Debug, Clone)]
pub struct ChatCommandActor {
    /// The invoking session's assigned user ID.
    pub user_id: u16,
    /// The invoking session's login name.
    pub login: String,
    /// See [`Access`].
    pub access: Access,
    /// See [`AccessExtra`].
    pub access_extra: AccessExtra,
    /// The prefix this command was typed behind; see
    /// [`crate::ServerConfig::console_prefix`].
    pub prefix: char,
}
