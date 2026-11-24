//! Account administration requests.

use hotline_proto::{Access, Field, FieldId, FieldList, Opcode, Received};

use crate::client::HotlineClient;
use crate::error::Result;

/// A user account's password, as read from or sent to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Password {
    /// No password.
    Empty,
    /// The password the server already has.
    Unchanged,
    /// A new password.
    New(Box<str>),
}

impl Password {
    /// The [`myField_UserPassword`] (106) field for this password.
    ///
    /// [`myField_UserPassword`]: hotline_proto::FieldId::UserPassword
    fn field(&self) -> Field {
        match self {
            Self::Empty => Field::string(FieldId::UserPassword, ""),
            Self::Unchanged => Field::bytes(FieldId::UserPassword, [0]),
            Self::New(text) => Field::obfuscated_string(FieldId::UserPassword, text.to_string()),
        }
    }
}

/// One user account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAccount {
    /// The name shown in the user list.
    pub name: Box<str>,
    /// The name the user logs in with.
    pub login: Box<str>,
    /// The password.
    pub password: Password,
    /// The privileges.
    pub access: Access,
}

impl UserAccount {
    /// The account in `fields`, as [`myTran_GetUser`] (352) or one record of
    /// [`myTran_GetUserList`] (348) sends it.
    ///
    /// [`myTran_GetUser`]: hotline_proto::Opcode::GetUser
    /// [`myTran_GetUserList`]: hotline_proto::Opcode::GetUserList
    fn from_fields(fields: &FieldList<Received>) -> Self {
        let has_password = fields
            .field(FieldId::UserPassword)
            .is_some_and(|f| !f.as_bytes().is_empty());
        Self {
            name: fields.string(FieldId::UserName).unwrap_or_default().into(),
            login: fields
                .field(FieldId::UserLogin)
                .map(|f| f.as_deobfuscated_string())
                .unwrap_or_default()
                .into(),
            password: if has_password {
                Password::Unchanged
            } else {
                Password::Empty
            },
            access: fields
                .field(FieldId::UserAccess)
                .and_then(Field::as_u64)
                .map_or_else(Access::empty, Access::from_bits_retain),
        }
    }

    /// The name, login, password, and access fields.
    fn fields(&self) -> [Field; 4] {
        [
            Field::string(FieldId::UserName, self.name.to_string()),
            Field::obfuscated_string(FieldId::UserLogin, self.login.to_string()),
            self.password.field(),
            Field::u64(FieldId::UserAccess, self.access.bits()),
        ]
    }
}

/// One change in a [`myTran_SetUserList`] (349) request.
///
/// [`myTran_SetUserList`]: hotline_proto::Opcode::SetUserList
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountChange {
    /// Create `account`, or replace the account `old_login`. A new login renames it.
    Save {
        /// The login before this change, `None` for a new account.
        old_login: Option<Box<str>>,
        /// The account after this change.
        account: UserAccount,
    },
    /// Delete the account with this login.
    Delete(Box<str>),
}

impl AccountChange {
    /// The fields of this change's record.
    fn fields(&self) -> Vec<Field> {
        match self {
            Self::Save { old_login, account } => old_login
                .iter()
                .map(|old| Field::obfuscated_string(FieldId::Data, old.to_string()))
                .chain(account.fields())
                .collect(),
            Self::Delete(login) => vec![Field::obfuscated_string(FieldId::Data, login.to_string())],
        }
    }
}

impl HotlineClient {
    /// Every account on the server, [`myTran_GetUserList`] (348).
    ///
    /// [`myTran_GetUserList`]: hotline_proto::Opcode::GetUserList
    pub async fn get_user_list(&self) -> Result<Vec<UserAccount>> {
        let reply = self.request(Opcode::GetUserList, Vec::new()).await?;
        Ok(reply
            .fields(FieldId::Data)
            .map(Field::as_field_list)
            .map(|record| record.map(|r| UserAccount::from_fields(&r)))
            .collect::<hotline_proto::Result<_>>()?)
    }

    /// Apply `changes` in one request, [`myTran_SetUserList`] (349).
    ///
    /// [`myTran_SetUserList`]: hotline_proto::Opcode::SetUserList
    pub async fn set_user_list(&self, changes: &[AccountChange]) -> Result<()> {
        let encoding = self.encoding().get();
        let fields = changes
            .iter()
            .map(|change| FieldList::encode(&change.fields(), encoding))
            .map(|record| record.map(|r| Field::bytes(FieldId::Data, r)))
            .collect::<hotline_proto::Result<_>>()?;
        self.request(Opcode::SetUserList, fields).await?;
        Ok(())
    }

    /// Create `account`, [`myTran_NewUser`] (350).
    ///
    /// [`myTran_NewUser`]: hotline_proto::Opcode::NewUser
    pub async fn new_user(&self, account: &UserAccount) -> Result<()> {
        self.request(Opcode::NewUser, account.fields().into())
            .await?;
        Ok(())
    }

    /// Delete the account `login`, [`myTran_DeleteUser`] (351).
    ///
    /// [`myTran_DeleteUser`]: hotline_proto::Opcode::DeleteUser
    pub async fn delete_user(&self, login: &str) -> Result<()> {
        let fields = vec![Field::obfuscated_string(
            FieldId::UserLogin,
            login.to_owned(),
        )];
        self.request(Opcode::DeleteUser, fields).await?;
        Ok(())
    }

    /// The account `login`, [`myTran_GetUser`] (352).
    ///
    /// [`myTran_GetUser`]: hotline_proto::Opcode::GetUser
    pub async fn get_user(&self, login: &str) -> Result<UserAccount> {
        let fields = vec![Field::string(FieldId::UserLogin, login.to_owned())];
        let reply = self.request(Opcode::GetUser, fields).await?;
        Ok(UserAccount::from_fields(&reply.fields))
    }

    /// Replace the account with `account`'s login, [`myTran_SetUser`] (353).
    ///
    /// [`myTran_SetUser`]: hotline_proto::Opcode::SetUser
    pub async fn set_user(&self, account: &UserAccount) -> Result<()> {
        self.request(Opcode::SetUser, account.fields().into())
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_keeps_an_access_bit_that_no_flag_defines() {
        let access = Access::SEND_CHAT | Access::from_bits_retain(1 << 51);
        let account = UserAccount {
            name: "Bob".into(),
            login: "bob".into(),
            password: Password::Empty,
            access,
        };
        let encoding = hotline_proto::text_encoding::DEFAULT;
        let record = FieldList::encode(&account.fields(), encoding).unwrap();
        let fields = FieldList::decode(&record, encoding).unwrap();

        assert_eq!(UserAccount::from_fields(&fields).access, access);
    }
}
