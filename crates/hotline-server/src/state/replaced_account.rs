use std::borrow::Cow;

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

/// Why [`crate::SharedState::replace_account`] did not save an account.
#[derive(Debug)]
pub(crate) struct NotReplaced {
    /// The error text for the reply.
    pub reason: Cow<'static, str>,
    /// The rename moved the account to the new login, then failed to save
    /// the edit.
    pub moved: bool,
}

impl From<&'static str> for NotReplaced {
    fn from(reason: &'static str) -> Self {
        Cow::from(reason).into()
    }
}

impl From<String> for NotReplaced {
    fn from(reason: String) -> Self {
        Cow::<'static, str>::from(reason).into()
    }
}

impl From<Cow<'static, str>> for NotReplaced {
    fn from(reason: Cow<'static, str>) -> Self {
        Self {
            reason,
            moved: false,
        }
    }
}
