use std::borrow::Cow;

use async_trait::async_trait;

use super::Account;

/// Account storage: looked up on login, mutated by the account CRUD
/// opcodes, [`myTran_NewUser`] (350), [`myTran_GetUser`] (352),
/// [`myTran_SetUser`] (353), and [`myTran_DeleteUser`] (351).
///
/// [`myTran_NewUser`]: hotline_proto::Opcode::NewUser
/// [`myTran_GetUser`]: hotline_proto::Opcode::GetUser
/// [`myTran_SetUser`]: hotline_proto::Opcode::SetUser
/// [`myTran_DeleteUser`]: hotline_proto::Opcode::DeleteUser
#[async_trait]
pub trait AccountProvider: Send + Sync {
    /// Look up an account by login.
    async fn get(&self, login: &str) -> Option<Account>;
    /// Create or overwrite an account.
    async fn put(&self, login: String, account: Account) -> Result<(), Cow<'static, str>>;
    /// Delete an account. `false` if it didn't exist.
    async fn delete(&self, login: &str) -> Result<bool, Cow<'static, str>>;
    /// Every account, login and details together, for bulk listing,
    /// [`myTran_GetUserList`] (348).
    ///
    /// [`myTran_GetUserList`]: hotline_proto::Opcode::GetUserList
    async fn list(&self) -> Vec<(String, Account)>;
}
