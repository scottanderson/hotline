use std::net::IpAddr;

/// One action by a logged-in session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEntry {
    /// The session's remote address.
    pub address: IpAddr,
    /// The session's account login.
    pub login: Box<str>,
    /// The session's nickname.
    pub name: Box<str>,
    /// What the session did.
    pub activity: Activity,
}

/// What a session did, one kind for each log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    /// Completed login.
    Connect,
    /// Started a file or folder download.
    Download {
        /// The file or folder name.
        name: Box<str>,
    },
    /// Started a file or folder upload.
    Upload {
        /// The file or folder name.
        name: Box<str>,
    },
    /// Changed an account.
    AccountChange {
        /// The kind of change.
        action: AccountAction,
        /// The account's login after the change.
        account: Box<str>,
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
        from: Box<str>,
    },
}

impl AccountAction {
    /// The action text in the log.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Create => "Create",
            Self::Delete => "Delete",
            Self::Modify => "Modify",
            Self::Rename { .. } => "Rename",
        }
    }
}
