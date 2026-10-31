use crate::account::Account;

/// An account that [`crate::SharedState::replace_account`] saved.
#[derive(Debug)]
pub(crate) struct ReplacedAccount {
    /// The saved account.
    pub account: Account,
    /// The rename's login was taken. The edit is written over that
    /// account, and the old login stays.
    pub over_taken_login: bool,
}
