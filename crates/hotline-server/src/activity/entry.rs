use std::net::IpAddr;

/// One action by a logged-in session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEntry {
    /// The session's remote address.
    pub address: IpAddr,
    /// The session's account login.
    pub login: String,
    /// The session's nickname.
    pub name: String,
    /// What the session did.
    pub activity: Activity,
}

/// What a session did, one kind for each of openline's logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    /// Completed login, openline's `LogConnect`.
    Connect,
    /// Started a file or folder download, openline's `LogDownload`.
    Download {
        /// The file or folder name.
        name: String,
    },
    /// Started a file or folder upload, openline's `LogUpload`.
    Upload {
        /// The file or folder name.
        name: String,
    },
    /// Changed an account, openline's `LogAccountChange`.
    AccountChange {
        /// The kind of change.
        action: AccountAction,
        /// The account's login after the change.
        account: String,
    },
}

/// The kind of account change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountAction {
    /// A new account.
    Create,
    /// A deleted account.
    Delete,
    /// A changed account.
    Modify,
    /// A changed login.
    Rename {
        /// The login before the change.
        from: String,
    },
}

impl AccountAction {
    /// openline's action text.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Create => "Create",
            Self::Delete => "Delete",
            Self::Modify => "Modify",
            Self::Rename { .. } => "Rename",
        }
    }
}
