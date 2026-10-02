//! Account administration requests.

use hotline_proto::{Access, Encoding, Field, FieldId, FieldList, Opcode};

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
            Self::New(password) => Field::obfuscated_string(FieldId::UserPassword, &**password),
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
    fn from_fields(fields: &FieldList, encoding: &'static Encoding) -> Self {
        let has_password = fields
            .field(FieldId::UserPassword)
            .is_some_and(|f| !f.as_bytes(encoding).is_empty());
        Self {
            name: fields
                .string(FieldId::UserName, encoding)
                .unwrap_or_default()
                .into(),
            login: fields
                .field(FieldId::UserLogin)
                .map(|f| f.as_deobfuscated_string(encoding))
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
                .map_or_else(Access::empty, Access::from_bits_truncate),
        }
    }

    /// The name, login, password, and access fields.
    fn fields(&self) -> [Field; 4] {
        [
            Field::string(FieldId::UserName, &*self.name),
            Field::obfuscated_string(FieldId::UserLogin, &*self.login),
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
                .map(|old| Field::obfuscated_string(FieldId::Data, &**old))
                .chain(account.fields())
                .collect(),
            Self::Delete(login) => vec![Field::obfuscated_string(FieldId::Data, &**login)],
        }
    }
}

impl HotlineClient {
    /// Every account on the server, [`myTran_GetUserList`] (348).
    ///
    /// [`myTran_GetUserList`]: hotline_proto::Opcode::GetUserList
    pub async fn get_user_list(&self) -> Result<Vec<UserAccount>> {
        let reply = self.request(Opcode::GetUserList, Vec::new()).await?;
        let encoding = self.encoding().get();
        Ok(reply
            .fields(FieldId::Data)
            .map(|f| FieldList::decode(&f.as_bytes(encoding)))
            .map(|record| record.map(|r| UserAccount::from_fields(&r, encoding)))
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
        let fields = vec![Field::obfuscated_string(FieldId::UserLogin, login)];
        self.request(Opcode::DeleteUser, fields).await?;
        Ok(())
    }

    /// The account `login`, [`myTran_GetUser`] (352).
    ///
    /// [`myTran_GetUser`]: hotline_proto::Opcode::GetUser
    pub async fn get_user(&self, login: &str) -> Result<UserAccount> {
        let fields = vec![Field::string(FieldId::UserLogin, login)];
        let reply = self.request(Opcode::GetUser, fields).await?;
        Ok(UserAccount::from_fields(
            &reply.fields,
            self.encoding().get(),
        ))
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
